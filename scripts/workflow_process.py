"""Lifecycle of one foreground verification, including nested recorders (POSIX)."""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid

SCOPE_ENV = 'PRAJNA_WORKFLOW_SCOPE'


class ProcessControlError(Exception):
    pass


def process_table():
    try:
        result = subprocess.run(['ps', '-axo', 'pid=,ppid=,pgid=,stat='],
                                capture_output=True, text=True, timeout=5)
        if result.returncode:
            raise ProcessControlError('process ownership inspection unavailable')
        return {int(pid): (int(parent), int(group), state) for pid, parent, group, state in
                (line.split() for line in result.stdout.splitlines())}
    except (OSError, ValueError, subprocess.TimeoutExpired):
        raise ProcessControlError('process ownership inspection unavailable') from None


def process_birth(pid):
    result = subprocess.run(['ps', '-o', 'lstart=', '-p', str(pid)], capture_output=True, text=True, timeout=5,
                            env={**os.environ, 'LC_ALL': 'C', 'TZ': 'UTC'})
    if result.returncode or not result.stdout.strip():
        raise ProcessControlError('verification group birth identity unavailable')
    return result.stdout.strip()


def descendants(table, seeds):
    owned = set(seeds)
    while True:
        more = {pid for pid, (parent, _, _) in table.items() if parent in owned} - owned
        if not more:
            return owned
        owned.update(more)


def send_group(group, sig):
    if group == os.getpgrp():
        raise ProcessControlError('refusing to signal supervisor group')
    try:
        os.killpg(group, sig)
    except ProcessLookupError:
        pass


class OwnedRun:
    """Private inherited registry retains nested group ownership after reparenting.

    Registry discovery is confined to the root's private directory. Process snapshots
    also capture detached foreground descendants such as Playwright's web server.
    No caller-supplied PID/group is accepted as a control target.
    """
    def __init__(self, command, cwd, env, stream):
        self.child = None
        self.groups = set()
        self.node = uuid.uuid4().hex
        self.root = None
        self.owner = False
        table = process_table()  # Fail before starting work when ownership is unobservable.
        context = env.get(SCOPE_ENV)
        if context:
            try:
                directory, parent = json.loads(context)
                self.root = Path(directory)
                info = self.root.stat()
                root_pid = json.loads((self.root / 'owner.json').read_text())['pid']
                if (info.st_uid != os.getuid() or info.st_mode & 0o077 or self.root.is_symlink()
                        or not isinstance(parent, str) or len(parent) != 32
                        or any(c not in '0123456789abcdef' for c in parent)
                        or not (self.root / f'{parent}.json').is_file()
                        or os.getpid() not in descendants(table, {root_pid})):
                    raise ValueError('unverified inherited scope')
            except (OSError, ValueError, KeyError, TypeError):
                raise ProcessControlError('unverified inherited verification scope') from None
        else:
            self.owner = True
            self.root = Path(tempfile.mkdtemp(prefix='prajna-verification-scope-'))
            (self.root / 'owner.json').write_text(json.dumps({'pid': os.getpid()}))
            parent = None
        self.parent = parent
        child_env = {**env, SCOPE_ENV: json.dumps([str(self.root), self.node])}
        read_fd, write_fd = os.pipe()
        try:
            # Exec is released only after the private ownership registration is visible.
            self.child = subprocess.Popen([sys.executable, str(Path(__file__).resolve()),
                                           '--child', str(read_fd), *command], cwd=cwd, env=child_env,
                                          stdout=stream, stderr=subprocess.STDOUT,
                                          pass_fds=(read_fd,), start_new_session=True)
            self.groups.add(self.child.pid)
            pending = self.root / f'{self.node}.pending'
            pending.write_text(json.dumps({'parent': parent, 'group': self.child.pid,
                                           'birth': process_birth(self.child.pid)}))
            pending.replace(self.root / f'{self.node}.json')
            os.write(write_fd, b'1')
        except BaseException:
            if self.child is not None:
                self.close()
            elif self.owner:
                shutil.rmtree(self.root)
            raise

        finally:
            os.close(read_fd)
            os.close(write_fd)

    def registered_groups(self):
        entries = {}
        for path in self.root.glob('*.json'):
            if path.name == 'owner.json':
                continue
            try:
                entries[path.stem] = json.loads(path.read_text())
            except FileNotFoundError:
                continue  # Another completed recorder already verified and removed its entry.
        nodes = {self.node}
        while True:
            more = {n for n, row in entries.items() if row['parent'] in nodes} - nodes
            if not more:
                break
            nodes.update(more)
        self.nodes = nodes
        groups = set()
        table = process_table()
        for n in nodes & entries.keys():
            row = entries[n]
            group = row['group']
            # Preserve ownership across reparenting, while detecting reused leader PIDs.
            # A leaderless group remains leased by its surviving group members.
            is_live_child = group == self.child.pid and self.child.poll() is None
            if group in table and not is_live_child:
                try:
                    birth = process_birth(group)
                except ProcessControlError:
                    if group in process_table():
                        raise
                else:
                    if birth != row['birth']:
                        self.groups.discard(group)  # It is now explicitly unowned, even on failure cleanup.
                        raise ProcessControlError('verification group identity changed')
            groups.add(group)
        return groups

    def close(self):
        """Freeze creators before TERM, then KILL survivors even if the leader exited."""
        try:
            # Freeze until discovery stabilizes, including registrations racing cancellation.
            stable = 0
            for _ in range(16):
                previous = set(self.groups)
                self.groups.update(self.registered_groups())
                table = process_table()
                seeds = {pid for pid, (_, group, _) in table.items() if group in self.groups}
                owned = descendants(table, seeds)
                self.groups.update(pid for pid in owned if table[pid][1] == pid)
                for group in self.groups:
                    send_group(group, signal.SIGSTOP)
                stable = stable + 1 if self.groups == previous else 0
                if stable >= 2:
                    break
            else:
                raise ProcessControlError('verification ownership did not stabilize')
            for group in self.groups:
                send_group(group, signal.SIGTERM)
                send_group(group, signal.SIGCONT)
            deadline = time.monotonic() + 0.5
            while True:
                table = process_table()
                alive = {group for _, group, state in table.values()
                         if group in self.groups and not state.startswith('Z')}
                if not alive or time.monotonic() >= deadline:
                    break
                time.sleep(0.02)
            for group in alive:
                send_group(group, signal.SIGKILL)
            if self.child is not None:
                self.child.wait(timeout=5)
            deadline = time.monotonic() + 2
            while any(group in self.groups and not state.startswith('Z')
                      for _, group, state in process_table().values()):
                if time.monotonic() >= deadline:
                    raise ProcessControlError('owned verification processes remain')
                time.sleep(0.02)
            for node in getattr(self, 'nodes', {self.node}):
                (self.root / f'{node}.json').unlink(missing_ok=True)
            if self.owner:
                shutil.rmtree(self.root)
        except (ProcessControlError, OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
            # Retain private ownership evidence and fail; still stop already confirmed groups.
            for group in self.groups:
                try:
                    send_group(group, signal.SIGKILL)
                except OSError:
                    pass
            if self.child is not None:
                try:
                    self.child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    pass
            raise ProcessControlError('verification cleanup unverified') from error


if __name__ == '__main__':
    # Private exec handshake, not a PID-control interface.
    if len(sys.argv) < 4 or sys.argv[1] != '--child':
        sys.exit(2)
    descriptor = int(sys.argv[2])
    released = os.read(descriptor, 1)
    os.close(descriptor)
    if released != b'1':
        sys.exit(127)
    os.execvpe(sys.argv[3], sys.argv[3:], os.environ)
