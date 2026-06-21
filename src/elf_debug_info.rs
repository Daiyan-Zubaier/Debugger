use anyhow::{Context, Result};
use gimli::{Dwarf, DwarfSections, EndianSlice, RunTimeEndian, SectionId};
use memmap2::Mmap;
use object::{Object, ObjectSection};
use self_cell::self_cell;
use std::{borrow::Cow, fs};

type Endian<'a> = EndianSlice<'a, RunTimeEndian>;

/// High-level interface for querying DWARF info
pub struct ElfDebugInfo {
  /// Self-referential cell that owns the memory map and parser
  inner: DebugInfoCell,
}

// Allows DebugParser to borrow from the mmap bytes
self_cell! {
  struct DebugInfoCell {
    owner: Mmap,

    // Borrows from owner
    #[covariant]
    dependent: DebugParser,
  }
}

/// Parser for object-file and DWARF data
pub struct DebugParser<'a> {
  /// Endianness of the object file
  pub endian: RunTimeEndian,

  /// Parsed object file, borrowed from the memory map
  pub obj: object::File<'a>,

  /// Raw DWARF section data such as .debug_info, .debug_str, and .debug_line
  pub dwarf_sections: DwarfSections<Cow<'a, [u8]>>,
}

impl ElfDebugInfo {
  /// Build an ELF/DWARF query helper for a binary
  pub fn new(binary_name: String) -> Result<Self> {
    let file = fs::File::open(&binary_name).with_context(|| format!("open {}", binary_name))?;

    // The debuggee binary is expected to stay unchanged while the debugger is using it
    let mmap = unsafe { Mmap::map(&file) }.with_context(|| format!("mmap {}", binary_name))?;

    let inner = DebugInfoCell::new(mmap, |mmap_bytes| {
      DebugParser::new(mmap_bytes).expect("DebugParser construction failed")
    });

    Ok(Self { inner })
  }

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

  /// Get the address ranges of the function containing `pc`
  pub fn get_function_ranges(&self, pc: u64) -> Result<Option<Vec<(u64, u64)>>> {
    self
      .inner
      .with_dependent(|_mmap, parser| parser.get_function_ranges(pc))
  }

  /// Get all line table addresses within function ranges, excluding the current address
  pub fn get_line_addresses_in_ranges(
    &self,
    ranges: &[(u64, u64)],
    exclude_addr: u64,
  ) -> Result<Vec<u64>> {
    self
      .inner
      .with_dependent(|_mmap, parser| parser.get_line_addresses_in_ranges(ranges, exclude_addr))
  }

  /// Convert a file name and line number to DWARF addresses
  pub fn file_line_to_addr(&self, filename: &str, line_num: u64) -> Result<Vec<u64>> {
    self
      .inner
      .with_dependent(|_mmap, parser| parser.file_line_to_addr(filename, line_num))
  }
}

impl<'a> DebugParser<'a> {
  /// Parse object and DWARF sections from mapped bytes
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

  /// Construct a temporary gimli::Dwarf view over the stored DWARF sections
  ///
  /// `gimli::Dwarf` does not own section data; it contains endian-aware readers that borrow from `dwarf_sections`
  ///
  /// Keeping it temporary avoids storing a value that borrows from temporary section views
  fn dwarf(&self) -> Dwarf<Endian<'_>> {
    self
      .dwarf_sections
      .borrow(|section| EndianSlice::new(section.as_ref(), self.endian))
  }

  /// Convert a program counter to a file and line
  pub fn pc_to_file_line(&self, pc: u64) -> Result<Option<(String, u64)>> {
    let dwarf = self.dwarf();
    let mut units = dwarf.units();

    while let Some(header) = units.next()? {
      let unit = dwarf.unit(header)?;

      // Find the CU DIE at depth 0 and check whether it covers this PC
      let mut entries = unit.entries();
      let mut cu_matches_pc = false;

      while let Some(entry) = entries.next_dfs()? {
        if entry.tag() == gimli::DW_TAG_compile_unit {
          cu_matches_pc = self.die_covers_pc(entry, &unit, &dwarf, pc)?;
          break;
        }
      }

      if !cu_matches_pc {
        continue;
      }

      // Pull the line program for this CU
      let Some(program) = unit.line_program.clone() else {
        continue;
      };

      // Resolve a DWARF string-like attribute into a Rust String
      let resolve_attr_string = |attr: gimli::AttributeValue<Endian<'_>>| -> Result<String> {
        let s = dwarf.attr_string(&unit, attr)?;
        Ok(s.to_string_lossy().into_owned())
      };

      // Walk line-table rows and keep the last row whose address is not past pc
      let mut rows = program.rows();
      let mut best: Option<(String, u64, u64)> = None;

      while let Some((line_header, row)) = rows.next_row()? {
        // End-of-sequence rows terminate one address range inside the line program
        if row.end_sequence() {
          let sequence_end = row.address();
          if pc < sequence_end
            && let Some((file, line, _addr)) = best.take()
          {
            return Ok(Some((file, line)));
          }
          best = None;
          continue;
        }

        let row_addr = row.address();

        // Addresses are monotonically increasing within a sequence
        if row_addr > pc {
          if let Some((file, line, _addr)) = best.take() {
            return Ok(Some((file, line)));
          }
          break;
        }

        // Ignore rows without source line information
        let Some(line_nz) = row.line() else {
          continue;
        };
        let line = line_nz.get();

        // Resolve file path for this row
        let Some(file_entry) = line_header.file(row.file_index()) else {
          continue;
        };

        // file_entry.path_name() is either absolute or relative to a directory entry
        let file_name = resolve_attr_string(file_entry.path_name())?;

        let full_path = if file_name.starts_with('/') {
          file_name
        } else {
          // directory_index == 0 means the compilation directory rather than include_directories
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

      // Otherwise, try next CU
    }

    Ok(None)
  }

  /// Convert a program counter to the containing function name
  pub fn pc_to_function(&self, pc: u64) -> Result<Option<String>> {
    let dwarf = self.dwarf();
    let mut units = dwarf.units();

    while let Some(header) = units.next()? {
      let unit = dwarf.unit(header)?;

      let mut entries = unit.entries();

      // Search subprogram DIEs for the first one covering this PC
      while let Some(entry) = entries.next_dfs()? {
        if entry.tag() == gimli::DW_TAG_subprogram
          && self.die_covers_pc(entry, &unit, &dwarf, pc)?
          && let Some(attr) = entry.attr(gimli::DW_AT_name)
        {
          let raw = dwarf.attr_string(&unit, attr.value())?;
          let name = raw.to_string_lossy().into_owned();

          return Ok(Some(name));
        }
      }
    }
    Ok(None)
  }

  /// Check whether a PC is covered by a DIE
  fn die_covers_pc(
    &self,
    entry: &gimli::DebuggingInformationEntry<Endian<'_>>,
    unit: &gimli::Unit<Endian<'_>>,
    dwarf: &Dwarf<Endian<'_>>,
    pc: u64,
  ) -> Result<bool> {
    let ranges = self.get_die_ranges(entry, unit, dwarf)?;
    for (low, high) in ranges {
      if pc >= low && pc < high {
        return Ok(true);
      }
    }
    Ok(false)
  }

  /// Extract address ranges from a DIE
  fn get_die_ranges(
    &self,
    entry: &gimli::DebuggingInformationEntry<Endian<'_>>,
    unit: &gimli::Unit<Endian<'_>>,
    dwarf: &Dwarf<Endian<'_>>,
  ) -> Result<Vec<(u64, u64)>> {
    // First try DW_AT_ranges for non-contiguous code
    if let Some(ranges_attr) = entry.attr_value(gimli::DW_AT_ranges) {
      let mut result = Vec::new();

      // Convert to RangeListsOffset for both DWARF 4 and 5
      let offset = match ranges_attr {
        gimli::AttributeValue::RangeListsRef(raw_offset) => {
          // DWARF 4 style
          gimli::RangeListsOffset(raw_offset.0)
        }
        gimli::AttributeValue::DebugRngListsIndex(idx) => {
          // DWARF 5 style
          match dwarf.ranges_offset(unit, idx) {
            Ok(o) => o,
            Err(_) => return Ok(vec![]),
          }
        }
        _ => return Ok(vec![]),
      };

      if let Ok(mut ranges) = dwarf.ranges(unit, offset) {
        while let Ok(Some(range)) = ranges.next() {
          if range.begin < range.end {
            result.push((range.begin, range.end));
          }
        }
      }

      if !result.is_empty() {
        return Ok(result);
      }
    }

    // Fall back to simple low_pc/high_pc
    let low_pc = match entry.attr_value(gimli::DW_AT_low_pc) {
      Some(gimli::AttributeValue::Addr(a)) => a,
      _ => return Ok(vec![]),
    };

    let high_attr = match entry.attr_value(gimli::DW_AT_high_pc) {
      Some(v) => v,
      None => return Ok(vec![]),
    };

    let high_pc = match high_attr {
      gimli::AttributeValue::Addr(a) => a,
      gimli::AttributeValue::Udata(off) => low_pc + off,
      _ => return Ok(vec![]),
    };

    Ok(vec![(low_pc, high_pc)])
  }

  /// Get the address ranges of the function containing `pc`
  pub fn get_function_ranges(&self, pc: u64) -> Result<Option<Vec<(u64, u64)>>> {
    let dwarf = self.dwarf();
    let mut units = dwarf.units();

    while let Some(header) = units.next()? {
      let unit = dwarf.unit(header)?;
      let mut entries = unit.entries();

      while let Some(entry) = entries.next_dfs()? {
        if entry.tag() == gimli::DW_TAG_subprogram {
          let ranges = self.get_die_ranges(entry, &unit, &dwarf)?;

          // Check if pc falls within any of the ranges
          for &(low, high) in &ranges {
            if pc >= low && pc < high {
              return Ok(Some(ranges));
            }
          }
        }
      }
    }
    Ok(None)
  }

  /// Get all unique line addresses within ranges, excluding a specific address
  pub fn get_line_addresses_in_ranges(
    &self,
    ranges: &[(u64, u64)],
    exclude_addr: u64,
  ) -> Result<Vec<u64>> {
    let dwarf = self.dwarf();
    let mut units = dwarf.units();
    let mut addresses = Vec::new();

    while let Some(header) = units.next()? {
      let unit = dwarf.unit(header)?;

      let Some(program) = unit.line_program.clone() else {
        continue;
      };

      let mut rows = program.rows();

      while let Some((_header, row)) = rows.next_row()? {
        if row.end_sequence() {
          continue;
        }

        let addr = row.address();

        // Check if address falls within any requested range
        let in_range = ranges.iter().any(|&(low, high)| addr >= low && addr < high);

        if in_range && addr != exclude_addr && !addresses.contains(&addr) {
          addresses.push(addr);
        }
      }
    }

    Ok(addresses)
  }

  /// Convert a file name and line number to matching addresses
  pub fn file_line_to_addr(&self, filename: &str, line_num: u64) -> Result<Vec<u64>> {
    let dwarf = self.dwarf();
    let mut units = dwarf.units();
    let mut addresses = Vec::new();

    while let Some(header) = units.next()? {
      let unit = dwarf.unit(header)?;

      let Some(program) = unit.line_program.clone() else {
        continue;
      };

      // Resolve a DWARF string-like attribute into a Rust String
      let resolve_attr_string = |attr: gimli::AttributeValue<Endian<'_>>| -> Result<String> {
        let s = dwarf.attr_string(&unit, attr)?;
        Ok(s.to_string_lossy().into_owned())
      };

      let mut rows = program.rows();

      while let Some((line_header, row)) = rows.next_row()? {
        if row.end_sequence() {
          continue;
        }

        // Check if line matches
        let Some(line_nz) = row.line() else {
          continue;
        };
        let row_line = line_nz.get();
        if row_line != line_num {
          continue;
        }

        // Resolve file path for this row
        let Some(file_entry) = line_header.file(row.file_index()) else {
          continue;
        };

        let file_name = resolve_attr_string(file_entry.path_name())?;

        // Build full path
        let full_path = if file_name.starts_with('/') {
          file_name.clone()
        } else {
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
            file_name.clone()
          } else if dir.ends_with('/') {
            format!("{dir}{file_name}")
          } else {
            format!("{dir}/{file_name}")
          }
        };

        // Check basename and full-path matches
        let matches =
          full_path.ends_with(filename) || file_name == filename || full_path == filename;

        if matches {
          let addr = row.address();
          if !addresses.contains(&addr) {
            addresses.push(addr);
          }
        }
      }
    }

    Ok(addresses)
  }
}
