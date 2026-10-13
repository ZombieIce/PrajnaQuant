import sys
import threading
import time
import unittest

import prajna


class PrajnaSmokeTests(unittest.TestCase):
    def test_import_exposes_rust_version_and_smoke_entry(self):
        self.assertEqual(prajna.__version__, "0.1.0")
        self.assertEqual(prajna.smoke_test(), "prajna-rust-ok")

    def test_python_thread_runs_during_long_rust_call(self):
        ready = threading.Event()
        progress: list[float] = []

        def python_work():
            ready.set()
            time.sleep(0.05)
            progress.append(time.monotonic())

        worker = threading.Thread(target=python_work)
        original_interval = sys.getswitchinterval()
        try:
            # Prevent Python preemption between the timestamps and the Rust call.
            sys.setswitchinterval(5.0)
            worker.start()
            self.assertTrue(ready.wait(timeout=5.0))
            started = time.monotonic()
            result = prajna.smoke_test(duration_ms=300)
            finished = time.monotonic()
        finally:
            sys.setswitchinterval(original_interval)
            worker.join(timeout=5.0)

        self.assertFalse(worker.is_alive())
        self.assertEqual(result, "prajna-rust-ok")
        self.assertGreaterEqual(finished - started, 0.3)
        self.assertEqual(len(progress), 1)
        self.assertGreater(progress[0], started)
        self.assertLess(progress[0], finished)

    def test_invalid_duration_raises_an_explicit_error(self):
        for value in (-1, 2**64):
            with self.subTest(value=value):
                with self.assertRaises(OverflowError):
                    prajna.smoke_test(value)
        for value in (0.5, "300", None):
            with self.subTest(value=value):
                with self.assertRaises(TypeError):
                    prajna.smoke_test(value)


if __name__ == "__main__":
    unittest.main()
