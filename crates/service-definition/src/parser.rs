use core::str;

use service_route::{SERVICE_ID_BYTES, ServiceId, ServiceIdError};

use crate::{
    MAX_ARGUMENT_BYTES, MAX_ARGUMENTS, MAX_DEFINITION_BYTES, MAX_DESCRIPTION_BYTES,
    MAX_EXECUTABLE_BYTES, MAX_NAME_BYTES, MAX_READY_MESSAGE_BYTES, MAX_RESTART_BACKOFF_YIELDS,
    MAX_RESTART_LIMIT, Readiness, RestartPolicy, SERVICE_DEFINITION_HEADER, ServiceDefinition,
};

const SERVICE_ID_TEXT_BYTES: usize = 36;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    ServiceId,
    Name,
    Description,
    Executable,
    Argument,
    Readiness,
    ReadyMessage,
    Restart,
    RestartLimit,
    RestartBackoffYields,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceIdTextError {
    InvalidLength,
    NonCanonical,
    Nil,
    InvalidVersion,
    InvalidVariant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    TooLarge,
    InvalidUtf8,
    MissingFinalNewline,
    CarriageReturn,
    InvalidHeader,
    InvalidLine { line: usize },
    UnknownField { line: usize },
    DuplicateField(Field),
    MissingField(Field),
    InvalidServiceId(ServiceIdTextError),
    InvalidName,
    InvalidDescription,
    InvalidExecutable,
    TooManyArguments,
    InvalidArgument,
    InvalidReadiness,
    InvalidReadyMessage,
    InvalidRestartPolicy,
    InvalidInteger(Field),
    ValueOutOfRange(Field),
    InconsistentReadiness,
    InconsistentRestartPolicy,
}

#[derive(Default)]
struct Seen {
    service_id: bool,
    name: bool,
    description: bool,
    executable: bool,
    readiness: bool,
    ready_message: bool,
    restart: bool,
    restart_limit: bool,
    restart_backoff_yields: bool,
}

pub fn parse(input: &[u8]) -> Result<ServiceDefinition<'_>, ParseError> {
    if input.is_empty() {
        return Err(ParseError::Empty);
    }
    if input.len() > MAX_DEFINITION_BYTES {
        return Err(ParseError::TooLarge);
    }
    if !input.ends_with(b"\n") {
        return Err(ParseError::MissingFinalNewline);
    }
    if input.contains(&b'\r') {
        return Err(ParseError::CarriageReturn);
    }
    let text = str::from_utf8(input).map_err(|_| ParseError::InvalidUtf8)?;
    let mut lines = text.split_terminator('\n');
    if lines.next() != Some(SERVICE_DEFINITION_HEADER) {
        return Err(ParseError::InvalidHeader);
    }

    let mut seen = Seen::default();
    let mut service_id = None;
    let mut name = None;
    let mut description = None;
    let mut executable = None;
    let mut arguments = [""; MAX_ARGUMENTS];
    let mut argument_count = 0;
    let mut readiness = None;
    let mut ready_message = None;
    let mut restart = None;
    let mut restart_limit = None;
    let mut restart_backoff_yields = None;

    for (line_index, line) in lines.enumerate() {
        let line_number = line_index + 2;
        if line.is_empty() || line.starts_with("# ") {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or(ParseError::InvalidLine { line: line_number })?;
        if key.is_empty() || value.is_empty() {
            return Err(ParseError::InvalidLine { line: line_number });
        }
        match key {
            "ServiceId" => {
                mark_once(&mut seen.service_id, Field::ServiceId)?;
                service_id = Some(parse_service_id(value)?);
            }
            "Name" => {
                mark_once(&mut seen.name, Field::Name)?;
                validate_name(value)?;
                name = Some(value);
            }
            "Description" => {
                mark_once(&mut seen.description, Field::Description)?;
                validate_text(value, MAX_DESCRIPTION_BYTES)
                    .map_err(|_| ParseError::InvalidDescription)?;
                description = Some(value);
            }
            "Executable" => {
                mark_once(&mut seen.executable, Field::Executable)?;
                validate_executable(value)?;
                executable = Some(value);
            }
            "Argument" => {
                if argument_count == MAX_ARGUMENTS {
                    return Err(ParseError::TooManyArguments);
                }
                validate_text(value, MAX_ARGUMENT_BYTES)
                    .map_err(|_| ParseError::InvalidArgument)?;
                arguments[argument_count] = value;
                argument_count += 1;
            }
            "Readiness" => {
                mark_once(&mut seen.readiness, Field::Readiness)?;
                readiness = Some(match value {
                    "immediate" => Readiness::Immediate,
                    "notify" => Readiness::Notify,
                    _ => return Err(ParseError::InvalidReadiness),
                });
            }
            "ReadyMessage" => {
                mark_once(&mut seen.ready_message, Field::ReadyMessage)?;
                validate_text(value, MAX_READY_MESSAGE_BYTES)
                    .map_err(|_| ParseError::InvalidReadyMessage)?;
                ready_message = Some(value);
            }
            "Restart" => {
                mark_once(&mut seen.restart, Field::Restart)?;
                restart = Some(match value {
                    "never" => RestartPolicy::Never,
                    "on-failure" => RestartPolicy::OnFailure,
                    "always" => RestartPolicy::Always,
                    _ => return Err(ParseError::InvalidRestartPolicy),
                });
            }
            "RestartLimit" => {
                mark_once(&mut seen.restart_limit, Field::RestartLimit)?;
                restart_limit = Some(parse_u32(value, Field::RestartLimit)?);
            }
            "RestartBackoffYields" => {
                mark_once(
                    &mut seen.restart_backoff_yields,
                    Field::RestartBackoffYields,
                )?;
                restart_backoff_yields = Some(parse_u32(value, Field::RestartBackoffYields)?);
            }
            _ => return Err(ParseError::UnknownField { line: line_number }),
        }
    }

    let service_id = required(service_id, Field::ServiceId)?;
    let name = required(name, Field::Name)?;
    let description = required(description, Field::Description)?;
    let executable = required(executable, Field::Executable)?;
    let readiness = required(readiness, Field::Readiness)?;
    let restart = required(restart, Field::Restart)?;
    let restart_limit = required(restart_limit, Field::RestartLimit)?;
    let restart_backoff_yields = required(restart_backoff_yields, Field::RestartBackoffYields)?;

    if restart_limit > MAX_RESTART_LIMIT {
        return Err(ParseError::ValueOutOfRange(Field::RestartLimit));
    }
    if restart_backoff_yields > MAX_RESTART_BACKOFF_YIELDS {
        return Err(ParseError::ValueOutOfRange(Field::RestartBackoffYields));
    }
    match (readiness, ready_message) {
        (Readiness::Notify, Some(_)) | (Readiness::Immediate, None) => {}
        _ => return Err(ParseError::InconsistentReadiness),
    }
    match restart {
        RestartPolicy::Never if restart_limit != 0 || restart_backoff_yields != 0 => {
            return Err(ParseError::InconsistentRestartPolicy);
        }
        RestartPolicy::OnFailure | RestartPolicy::Always if restart_limit == 0 => {
            return Err(ParseError::InconsistentRestartPolicy);
        }
        _ => {}
    }

    Ok(ServiceDefinition::new(
        service_id,
        name,
        description,
        executable,
        arguments,
        argument_count,
        readiness,
        ready_message,
        restart,
        restart_limit,
        restart_backoff_yields,
    ))
}

fn required<T>(value: Option<T>, field: Field) -> Result<T, ParseError> {
    value.ok_or(ParseError::MissingField(field))
}

fn mark_once(seen: &mut bool, field: Field) -> Result<(), ParseError> {
    if *seen {
        return Err(ParseError::DuplicateField(field));
    }
    *seen = true;
    Ok(())
}

fn validate_name(value: &str) -> Result<(), ParseError> {
    if value.is_empty() || value.len() > MAX_NAME_BYTES {
        return Err(ParseError::InvalidName);
    }
    for component in value.split('.') {
        let bytes = component.as_bytes();
        if bytes.is_empty()
            || !bytes[0].is_ascii_lowercase()
            || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
            || bytes
                .iter()
                .copied()
                .any(|byte| !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'))
        {
            return Err(ParseError::InvalidName);
        }
    }
    Ok(())
}

fn validate_executable(value: &str) -> Result<(), ParseError> {
    if value.len() < 2
        || value.len() > MAX_EXECUTABLE_BYTES
        || !value.starts_with('/')
        || value.ends_with('/')
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(ParseError::InvalidExecutable);
    }
    if value[1..]
        .split('/')
        .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(ParseError::InvalidExecutable);
    }
    Ok(())
}

fn validate_text(value: &str, maximum: usize) -> Result<(), ()> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(());
    }
    Ok(())
}

fn parse_u32(value: &str, field: Field) -> Result<u32, ParseError> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(ParseError::InvalidInteger(field));
    }
    let mut result = 0_u32;
    for byte in value.bytes() {
        result = result
            .checked_mul(10)
            .and_then(|current| current.checked_add(u32::from(byte - b'0')))
            .ok_or(ParseError::ValueOutOfRange(field))?;
    }
    Ok(result)
}

fn parse_service_id(value: &str) -> Result<ServiceId, ParseError> {
    if value.len() != SERVICE_ID_TEXT_BYTES {
        return Err(ParseError::InvalidServiceId(
            ServiceIdTextError::InvalidLength,
        ));
    }
    let source = value.as_bytes();
    if source[8] != b'-' || source[13] != b'-' || source[18] != b'-' || source[23] != b'-' {
        return Err(ParseError::InvalidServiceId(
            ServiceIdTextError::NonCanonical,
        ));
    }
    let mut bytes = [0_u8; SERVICE_ID_BYTES];
    let mut source_index = 0;
    let mut output_index = 0;
    while output_index < bytes.len() {
        if matches!(source_index, 8 | 13 | 18 | 23) {
            source_index += 1;
        }
        let high = decode_lower_hex(source[source_index]).ok_or(ParseError::InvalidServiceId(
            ServiceIdTextError::NonCanonical,
        ))?;
        let low = decode_lower_hex(source[source_index + 1]).ok_or(
            ParseError::InvalidServiceId(ServiceIdTextError::NonCanonical),
        )?;
        bytes[output_index] = high << 4 | low;
        source_index += 2;
        output_index += 1;
    }
    ServiceId::from_bytes(bytes).map_err(|error| {
        ParseError::InvalidServiceId(match error {
            ServiceIdError::Nil => ServiceIdTextError::Nil,
            ServiceIdError::InvalidVersion => ServiceIdTextError::InvalidVersion,
            ServiceIdError::InvalidVariant => ServiceIdTextError::InvalidVariant,
        })
    })
}

const fn decode_lower_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
