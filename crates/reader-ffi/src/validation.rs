use crate::wire::{Command, Error, Request, Result, VERSION};

pub fn decode(input: &[u8]) -> Result<Command> {
    let request: Request = serde_json::from_slice(input).map_err(|_| {
        Error::new(
            "invalid_request",
            "Expected valid UTF-8 and ABI v1 JSON fields",
        )
    })?;
    if request.version != VERSION {
        return Err(Error::new(
            "unsupported_version",
            "Supported ABI version is 1",
        ));
    }
    validate(&request.command)?;
    Ok(request.command)
}

fn validate(command: &Command) -> Result<()> {
    match command {
        Command::Open { path } if path.is_empty() || path.len() > 4096 || path.contains('\0') => {
            Err(Error::new(
                "invalid_argument",
                "Database path must be nonempty and bounded",
            ))
        }
        Command::Import { bytes, name, .. }
        | Command::ImportImage { bytes, name, .. }
        | Command::ImportScannedPdf { bytes, name, .. }
            if bytes.is_empty()
                || bytes.len() > reader_core::MAX_IMPORT
                || name.is_empty()
                || name.len() > 4096 =>
        {
            Err(Error::new(
                "invalid_argument",
                "Import must contain 1..20MiB and a bounded filename",
            ))
        }
        Command::Review {
            direction, grade, ..
        } if !["forward", "reverse"].contains(&direction.as_str()) || *grade > 3 => Err(
            Error::new("invalid_argument", "Review direction or grade is invalid"),
        ),
        _ => Ok(()),
    }
}
