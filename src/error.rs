//! Some core error/result structures.

#[macro_export]
macro_rules! versio_err {
  ($($arg:tt)*) => (crate::error::Error::new(format!($($arg)*)))
}

#[derive(Debug)]
pub struct Error {
  description: String
}

impl From<std::num::ParseIntError> for Error {
  fn from(err: std::num::ParseIntError) -> Error { Error { description: err.to_string() } }
}

impl From<std::io::Error> for Error {
  fn from(err: std::io::Error) -> Error { Error { description: format!("io {:?}", err) } }
}

impl From<git2::Error> for Error {
  fn from(err: git2::Error) -> Error { Error { description: format!("git {:?}", err) } }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_construct_err() { let _e: Error = Error { description: "This is a test.".into() }; }

  #[test]
  fn test_debug_err() { let _e: String = format!("Error: {:?}", Error { description: "This is a test.".into() }); }

  #[test]
  fn test_parse_err() { let _e: Error = "not a number".parse::<u32>().unwrap_err().into(); }

  #[test]
  fn test_io_err() {
    use std::io::{Error as IoError, ErrorKind};
    let _e: Error = IoError::new(ErrorKind::Other, "test error").into();
  }
}
