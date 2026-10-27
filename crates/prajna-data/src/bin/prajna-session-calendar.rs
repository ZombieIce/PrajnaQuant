use std::{path::PathBuf, process::ExitCode};

use prajna_data::session_calendar::{CalendarError, CalendarStore};

fn main() -> ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(()) => {
            println!("Calendar version imported; receipt saved locally.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            // Parsing/I/O messages can contain local paths or input values.
            let message = match &error {
                CalendarError::Io(_) => "local calendar I/O failed".to_owned(),
                CalendarError::Json(_) => "invalid calendar JSON or field value".to_owned(),
                _ => error.to_string(),
            };
            eprintln!("Calendar import failed: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(mut args: impl Iterator<Item = String>) -> Result<(), CalendarError> {
    let (mut root, mut input, mut receipt) = (None, None, None);
    if args.next().as_deref() != Some("import") {
        return Err(CalendarError::Invalid(
            "usage: import --root PATH --input FILE --receipt NAME",
        ));
    }
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or(CalendarError::Invalid("missing option value"))?;
        let target = match flag.as_str() {
            "--root" => &mut root,
            "--input" => &mut input,
            "--receipt" => &mut receipt,
            _ => return Err(CalendarError::Invalid("unknown calendar option")),
        };
        if target.replace(value).is_some() {
            return Err(CalendarError::Invalid("duplicate calendar option"));
        }
    }
    let root = PathBuf::from(root.ok_or(CalendarError::Invalid("local root is required"))?);
    let input = input.ok_or(CalendarError::Invalid("input file is required"))?;
    let receipt = receipt.ok_or(CalendarError::Invalid("receipt name is required"))?;
    let store = CalendarStore::open(root)?;
    let version = store.import_file(input)?;
    store.write_receipt(&receipt, &version)
}
