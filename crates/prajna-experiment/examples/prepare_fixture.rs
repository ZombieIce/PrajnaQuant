//! README setup: publish synthetic data, save Universe, write experiment.json.
mod support {
    pub mod fixture;
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: prepare_fixture <lake> <experiment.json>".into());
    }
    let definition = support::fixture::prepare(args[0].as_ref(), args[1].as_ref())?;
    println!("{definition}");
    Ok(())
}
