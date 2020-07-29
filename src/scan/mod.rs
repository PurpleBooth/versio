mod json;
pub mod parts;
mod toml;
mod xml;
mod yaml;

pub use self::toml::TomlScanner;
pub use self::xml::XmlScanner;
use crate::error::Result;
use crate::source::{Mark, MarkedData, NamedData};
pub use json::JsonScanner;
pub use yaml::YamlScanner;
use crate::scan::parts::Part;

pub trait Scanner {
  fn build(parts: Vec<Part>) -> Self;

  fn find(&self, data: &str) -> Result<Mark>;

  fn scan(&self, data: NamedData) -> Result<MarkedData> {
    let mark = self.find(data.data())?;
    Ok(data.mark(mark))
  }
}
