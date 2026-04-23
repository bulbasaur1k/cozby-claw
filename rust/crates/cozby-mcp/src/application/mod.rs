//! Application layer. Описывает порты (trait-интерфейсы), от которых зависят
//! use-case'ы. Никакой реальной файловой системы здесь нет — только абстракции.

pub mod ports;
pub mod use_cases;

pub use ports::{DirEntry, DirEntryKind, FileSystem, ReadOutcome};
