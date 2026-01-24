use anyhow::{Context, Result};
use gimli::{Dwarf, DwarfSections, EndianSlice, RunTimeEndian, SectionId};
use memmap2::Mmap;
use object::{Object, ObjectSection};
use self_cell::self_cell;
use std::{borrow::Cow, fs};

type Endian<'a> = EndianSlice<'a, RunTimeEndian>;

/// High-level interface for querying dwarf info
pub struct ElfDebugInfo {
  /// ELF binary path
  binary_name: String,

  /// Self referential that owns the memory map and its dependent struct DebugParser
  inner: DebugInfoCell,
}

// Allows us to have DebugParser borrow from MMap
self_cell! {
  struct DebugInfoCell {
    owner: Mmap,

    // Borrows from owner (the mmap bytes)
    #[covariant]
    dependent: DebugParser,
  }
}

/// Struct is responsible for handling the underlying parsing logic
/// It is abstracted away by ElfDebugInfo
pub struct DebugParser<'a> {
  /// Holds the endianess of the program
  pub endian: RunTimeEndian,

  /// Parsed object file, borrows from memory map
  pub obj: object::File<'a>,

  /// Raw dwarf section data (ex: .debug_info, .debug_str, .debug_line)
  pub dwarf_sections: DwarfSections<Cow<'a, [u8]>>,
}

impl ElfDebugInfo {
  /// Construct ElfDebugInfo object
  pub fn new(binary_name: String) -> Result<Self> {
    let file = fs::File::open(&binary_name).with_context(|| format!("open {}", binary_name))?;

    // mmap requires you to ensure the file isn't modified while mapped
    // In our case doesn't really matter since during a typical debugging process file isn't really modified
    let mmap = unsafe { Mmap::map(&file) }.with_context(|| format!("mmap {}", binary_name))?;

    let inner = DebugInfoCell::new(mmap, |mmap_bytes| {
      DebugParser::new(mmap_bytes).expect("DebugParser construction failed")
    });

    Ok(Self { binary_name, inner })
  }

  // Public API delegates to parser:

  pub fn pc_to_file_line(&self, pc: u64) -> Result<Option<(String, u64)>> {
    self
      .inner
      .with_dependent(|_mmap, parser| parser.pc_to_file_line(pc))
  }

  pub fn pc_to_function(&self, pc: u64) -> Result<Option<String>> {
    self
      .inner
      .with_dependent(|_mmap, parser| parser.pc_to_function(pc))
  }
}

impl<'a> DebugParser<'a> {
  /// Construct new DebugParser object
  pub fn new(bytes: &'a [u8]) -> Result<Self> {
    let obj = object::File::parse(bytes).context("parse object file")?;

    let endian = if obj.is_little_endian() {
      RunTimeEndian::Little
    } else {
      RunTimeEndian::Big
    };

    // Load DWARF sections through object
    let load_section = |id: SectionId| -> Result<Cow<'a, [u8]>> {
      let name = id.name();
      Ok(match obj.section_by_name(name) {
        Some(section) => section.uncompressed_data()?,
        None => Cow::Borrowed(&[]),
      })
    };

    let dwarf_sections = DwarfSections::load(&load_section)?;

    Ok(Self {
      endian,
      obj,
      dwarf_sections,
    })
  }

  /// Constructs a temporary gimli::Dwarf view over the stored DWARF sections.
  ///
  /// `gimli::Dwarf` does NOT own section data; it contains endian-aware readers (`EndianSlice`) that borrow from `dwarf_sections`.
  ///
  /// A previous implementation attempted to store `Dwarf` as a struct field, but this was invalid because `Dwarf` borrows
  /// from temporary `DwarfSections` values created during construction.
  fn dwarf(&self) -> Dwarf<Endian<'_>> {
    self
      .dwarf_sections
      .borrow(|section| EndianSlice::new(section.as_ref(), self.endian))
  }

  /// Takes program counter and returns file name and line number
  pub fn pc_to_file_line(&self, pc: u64) -> Result<Option<(String, u64)>> {
    let dwarf = self.dwarf();
    let mut units = dwarf.units();

    while let Some(header) = units.next()? {
      let unit = dwarf.unit(header)?;

      // Find the CU DIE (depth 0) and reuse your existing range check.
      let mut entries = unit.entries();
      let mut cu_matches_pc = false;

      while let Some((depth, entry)) = entries.next_dfs()? {
        if depth == 0 && entry.tag() == gimli::DW_TAG_compile_unit {
          cu_matches_pc = self.die_covers_pc(entry, pc)?;
          break;
        }
      }

      if !cu_matches_pc {
        continue;
      }

      // Pull the line program for this CU.
      let Some(program) = unit.line_program.clone() else {
        continue;
      };

      // Helper: resolve a DWARF "string-ish" AttributeValue into a Rust String.
      // Works for debug_str refs, inline strings, etc.
      let mut resolve_attr_string = |attr: gimli::AttributeValue<Endian<'_>>| -> Result<String> {
        let s = dwarf.attr_string(&unit, attr)?;
        Ok(s.to_string_lossy().into_owned())
      };

      // Walk the line table rows and keep the last row whose address <= pc
      // within the current sequence.
      let mut rows = program.rows();
      let mut best: Option<(String, u64, u64)> = None; // (file, line, row_addr)

      while let Some((line_header, row)) = rows.next_row()? {
        // If we hit the end of a sequence:
        // - if we already have a best row for this sequence, that’s the answer
        // - otherwise reset and keep scanning (some CUs have multiple sequences)
        if row.end_sequence() {
          if let Some((file, line, _addr)) = best.take() {
            return Ok(Some((file, line)));
          }
          continue;
        }

        let row_addr = row.address();

        // Addresses are monotonically increasing within a sequence.
        // Once we pass `pc`, we should return the last best row we saw.
        if row_addr > pc {
          break;
        }

        // Some rows might not have a line (or might be "line = 0"); ignore those.
        let Some(line_nz) = row.line() else {
          continue;
        };
        let line = line_nz.get();

        // Resolve file path for this row.
        let Some(file_entry) = line_header.file(row.file_index()) else {
          continue;
        };

        // file_entry.path_name() is either an absolute path or relative-to-directory.
        let file_name = resolve_attr_string(file_entry.path_name())?;

        let full_path = if file_name.starts_with('/') {
          file_name
        } else {
          // directory_index == 0 means “compilation directory” (not in include_directories table)
          // We can still try to join include_directories when index != 0.
          let dir = if file_entry.directory_index() != 0 {
            if let Some(dir_attr) = line_header.directory(file_entry.directory_index()) {
              resolve_attr_string(dir_attr)?
            } else {
              String::new()
            }
          } else {
            String::new()
          };

          if dir.is_empty() {
            file_name
          } else if dir.ends_with('/') {
            format!("{dir}{file_name}")
          } else {
            format!("{dir}/{file_name}")
          }
        };

        best = Some((full_path, line, row_addr));
      }

      // If we broke out (row_addr > pc) but we have a best row, use it.
      if let Some((file, line, _addr)) = best.take() {
        return Ok(Some((file, line)));
      }

      // Otherwise, try next CU.
    }

    Ok(None)
  }

  /// Takes program counter and returns the function it is in
  pub fn pc_to_function(&self, pc: u64) -> Result<Option<String>> {
    let dwarf = self.dwarf();
    let mut units = dwarf.units();

    // Loop through the compile units
    while let Some(header) = units.next()? {
      // Parse unit
      let unit = dwarf.unit(header)?;

      let mut entries = unit.entries();

      // Run a DFS search, depth represents the hierchal structure, entry is the actual info
      while let Some((_depth, entry)) = entries.next_dfs()? {
        if entry.tag() == gimli::DW_TAG_subprogram {
          if self.die_covers_pc(entry, pc)? {
            if let Some(attr) = entry.attr(gimli::DW_AT_name)? {
              let raw = dwarf.attr_string(&unit, attr.value())?; // Raw bytes
              let name = raw.to_string_lossy().into_owned(); // Converts raw bytes to string

              return Ok(Some(name));
            }
          }
        }
      }
    }
    Ok(None)
  }

  /// Check if PC is in current DIE
  /// Returns true if it is, false otherwise
  fn die_covers_pc(
    &self,
    entry: &gimli::DebuggingInformationEntry<Endian<'_>>,
    pc: u64,
  ) -> Result<bool> {
    let low_pc = match entry.attr_value(gimli::DW_AT_low_pc)? {
      Some(gimli::AttributeValue::Addr(a)) => a,
      _ => return Ok(false), // no low_pc => can't use this simple path
    };

    let high_attr = match entry.attr_value(gimli::DW_AT_high_pc)? {
      Some(v) => v,
      None => return Ok(false),
    };

    let high_pc = match high_attr {
      gimli::AttributeValue::Addr(a) => a,
      gimli::AttributeValue::Udata(off) => low_pc + off,
      _ => return Ok(false),
    };

    if pc >= low_pc && pc < high_pc {
      return Ok(true);
    }

    Ok(false)
  }
}
