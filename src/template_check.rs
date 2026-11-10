//! Bounded checks of explicit Markdown structure, not rendered or semantic equivalence.

use crate::error::{Result, invalid};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};

const MAX_INPUT_BYTES: usize = 256 * 1024;

#[derive(Default)]
struct Section {
    line: usize,
    parent: usize,
    level: usize,
    title: String,
    tables: Vec<Table>,
    checklist_count: usize,
    checklist_line: usize,
}

struct Table {
    line: usize,
    headers: Vec<String>,
}

struct Structure {
    sections: Vec<Section>,
}

/// Compare the supported, explicit structure of a template and a filled document.
///
/// Each input is limited to 256 KiB and must not contain NUL. A template must
/// contain at least one supported heading, table, or checklist item. Differences
/// return `Ok` with `passed: false` and structured `missing` entries; invalid
/// inputs return the crate's validation error. See `docs/TEMPLATE-CHECK.md` for
/// the deliberately limited Markdown syntax and matching rules.
pub fn check(template: &str, document: &str) -> Result<Value> {
    validate_input("template", template)?;
    validate_input("document", document)?;
    let (skeleton, skeleton_line, template_source) = skeleton(template)?;
    let mut template = parse(&skeleton);
    for section in &mut template.sections {
        if section.line > 0 {
            section.line += skeleton_line;
        }
        if section.checklist_line > 0 {
            section.checklist_line += skeleton_line;
        }
        for table in &mut section.tables {
            table.line += skeleton_line;
        }
    }
    let document = parse(document);
    let required = counts(&template);
    if required == [0, 0, 0] {
        return Err(invalid(
            "template must contain a supported ATX heading, table header, or checklist item",
        ));
    }

    let mut heading_positions: HashMap<_, VecDeque<_>> = HashMap::new();
    for (index, section) in document.sections.iter().enumerate().skip(1) {
        heading_positions
            .entry((section.parent, section.level, section.title.as_str()))
            .or_default()
            .push_back(index);
    }
    let mut matched_sections = vec![None; template.sections.len()];
    matched_sections[0] = Some(0);
    let mut cursors = vec![0; template.sections.len()];
    let mut matched = [0; 3];
    let mut missing = Vec::new();
    let placeholder_count = template
        .sections
        .iter()
        .filter(|s| s.title.contains('{'))
        .count();
    if placeholder_count > 64 {
        return Err(invalid(
            "template allows at most 64 headings containing placeholders",
        ));
    }

    for (index, section) in template.sections.iter().enumerate() {
        if index != 0 {
            let pattern = placeholder_pattern(&section.title);
            if let (Some(parent), Some(pattern)) =
                (matched_sections[section.parent], pattern.as_ref())
            {
                matched_sections[index] = document
                    .sections
                    .iter()
                    .enumerate()
                    .skip(cursors[section.parent].max(1))
                    .find(|(_, actual)| {
                        actual.parent == parent
                            && actual.level == section.level
                            && pattern.is_match(&actual.title)
                    })
                    .map(|(position, _)| position);
                if let Some(position) = matched_sections[index] {
                    cursors[section.parent] = position + 1;
                    matched[0] += 1;
                }
            }
            if pattern.is_none()
                && let Some(parent) = matched_sections[section.parent]
                && let Some(positions) =
                    heading_positions.get_mut(&(parent, section.level, section.title.as_str()))
            {
                while positions
                    .front()
                    .is_some_and(|position| *position < cursors[section.parent])
                {
                    positions.pop_front();
                }
                matched_sections[index] = positions.pop_front();
                if let Some(position) = matched_sections[index] {
                    cursors[section.parent] = position + 1;
                    matched[0] += 1;
                }
            }
            if matched_sections[index].is_none() {
                missing.push(json!({
                    "kind": "heading",
                    "template_line": section.line,
                    "parent_template_line": template.sections[section.parent].line,
                    "level": section.level,
                    "text": section.title,
                }));
            }
        }

        let actual = matched_sections[index].map(|position| &document.sections[position]);
        let mut table_positions: HashMap<_, VecDeque<_>> = HashMap::new();
        if let Some(actual) = actual {
            for (position, table) in actual.tables.iter().enumerate() {
                table_positions
                    .entry(table.headers.as_slice())
                    .or_default()
                    .push_back(position);
            }
        }
        let mut table_cursor = 0;
        for table in &section.tables {
            let found = table_positions
                .get_mut(table.headers.as_slice())
                .and_then(|positions| {
                    while positions.front().is_some_and(|p| *p < table_cursor) {
                        positions.pop_front();
                    }
                    positions.pop_front()
                });
            if let Some(position) = found {
                table_cursor = position + 1;
                matched[1] += 1;
            } else {
                missing.push(json!({
                    "kind": "table_header",
                    "template_line": table.line,
                    "section_template_line": section.line,
                    "headers": table.headers,
                }));
            }
        }

        let found = actual.map_or(0, |section| section.checklist_count);
        matched[2] += found.min(section.checklist_count);
        if found < section.checklist_count {
            missing.push(json!({
                "kind": "checklist",
                "template_line": section.checklist_line,
                "section_template_line": section.line,
                "required": section.checklist_count,
                "found": found,
                "missing": section.checklist_count - found,
            }));
        }
    }

    Ok(json!({
        "check": "markdown_template_structure",
        "version": 1,
        "template_source":template_source,
        "passed": missing.is_empty(),
        "structural_only": true,
        "semantic_guarantee": false,
        "summary": {
            "headings": {"required": required[0], "matched": matched[0]},
            "tables": {"required": required[1], "matched": matched[1]},
            "checklist_items": {"required": required[2], "matched": matched[2]},
        },
        "missing": missing,
    }))
}

/// Recognize an explicitly marked skeleton, or the first markdown fence inside
/// a Chinese "模板结构" section. Other fences remain examples and are ignored.
fn skeleton(input: &str) -> Result<(String, usize, &'static str)> {
    let lines: Vec<_> = input.lines().collect();
    let start_marker = "<!-- HARNESS-TEMPLATE-START -->";
    let end_marker = "<!-- HARNESS-TEMPLATE-END -->";
    if let Some(start) = lines.iter().position(|l| l.trim() == start_marker) {
        let end = lines
            .iter()
            .enumerate()
            .skip(start + 1)
            .find(|(_, l)| l.trim() == end_marker)
            .map(|(i, _)| i)
            .ok_or_else(|| invalid("unclosed HARNESS-TEMPLATE skeleton"))?;
        return Ok((
            lines[start + 1..end].join("\n"),
            start + 1,
            "marked_skeleton",
        ));
    }
    let mut in_fence = None;
    let mut section_level = None;
    for (i, raw) in lines.iter().enumerate() {
        let Some(line) = block_line(raw) else {
            continue;
        };
        if let Some((marker, length)) = in_fence {
            if closes_fence(line, marker, length) {
                in_fence = None;
            }
            continue;
        }
        if let Some((level, title)) = heading(line) {
            if title.ends_with("模板结构") {
                section_level = Some(level);
            } else if section_level.is_some_and(|l| level <= l) {
                section_level = None;
            }
        }
        if let Some((marker, length)) = opens_fence(line) {
            let info = line.trim_start_matches(marker as char).trim();
            if section_level.is_some() && matches!(info, "markdown" | "md") {
                let end = lines
                    .iter()
                    .enumerate()
                    .skip(i + 1)
                    .find(|(_, l)| block_line(l).is_some_and(|l| closes_fence(l, marker, length)))
                    .map(|(i, _)| i)
                    .ok_or_else(|| invalid("unclosed markdown template skeleton"))?;
                return Ok((lines[i + 1..end].join("\n"), i + 1, "markdown_skeleton"));
            }
            in_fence = Some((marker, length));
        }
    }
    Ok((input.to_owned(), 0, "whole_document"))
}

fn placeholder_pattern(title: &str) -> Option<regex::Regex> {
    let placeholders = regex::Regex::new(r"\{[^{}\n]+\}").unwrap();
    let mut pattern = String::from("^");
    let mut end = 0;
    for capture in placeholders.find_iter(title) {
        pattern.push_str(&regex::escape(&title[end..capture.start()]));
        pattern.push_str(".+?");
        end = capture.end();
    }
    if end == 0 {
        return None;
    }
    pattern.push_str(&regex::escape(&title[end..]));
    pattern.push('$');
    regex::Regex::new(&pattern).ok()
}

fn validate_input(name: &str, value: &str) -> Result<()> {
    if value.len() > MAX_INPUT_BYTES {
        return Err(invalid(format!("{name} exceeds 256 KiB")));
    }
    if value.contains('\0') {
        return Err(invalid(format!("{name} contains NUL")));
    }
    Ok(())
}

fn counts(structure: &Structure) -> [usize; 3] {
    [
        structure.sections.len() - 1,
        structure.sections.iter().map(|s| s.tables.len()).sum(),
        structure.sections.iter().map(|s| s.checklist_count).sum(),
    ]
}

fn parse(input: &str) -> Structure {
    let lines: Vec<_> = input.lines().collect();
    let mut structure = Structure {
        sections: vec![Section::default()],
    };
    let mut ancestors = vec![0];
    let mut current = 0;
    let mut fence: Option<(u8, usize)> = None;
    let mut in_table = false;
    let mut index = 0;
    while index < lines.len() {
        let line_number = index + 1;
        let raw = lines[index];
        index += 1;
        let Some(line) = block_line(raw) else {
            in_table = false;
            continue;
        };
        if let Some((marker, length)) = fence {
            if closes_fence(line, marker, length) {
                fence = None;
            }
            continue;
        }
        if let Some(opening) = opens_fence(line) {
            fence = Some(opening);
            in_table = false;
            continue;
        }
        if let Some((level, title)) = heading(line) {
            while structure.sections[*ancestors.last().unwrap()].level >= level {
                ancestors.pop();
            }
            let parent = *ancestors.last().unwrap();
            current = structure.sections.len();
            structure.sections.push(Section {
                line: line_number,
                parent,
                level,
                title,
                ..Section::default()
            });
            ancestors.push(current);
            in_table = false;
            continue;
        }
        if in_table && table_cells(line).is_some() {
            continue;
        }
        in_table = false;
        if let Some(headers) = table_cells(line)
            && let Some(separator) = lines.get(index).and_then(|line| block_line(line))
            && let Some(cells) = table_cells(separator)
            && cells.len() == headers.len()
            && cells.iter().all(|cell| separator_cell(cell))
        {
            structure.sections[current].tables.push(Table {
                line: line_number,
                headers,
            });
            index += 1;
            in_table = true;
            continue;
        }
        if checklist_item(line) {
            let section = &mut structure.sections[current];
            section.checklist_count += 1;
            if section.checklist_line == 0 {
                section.checklist_line = line_number;
            }
        }
    }
    structure
}

// Only recognize standalone blocks indented by at most three ASCII spaces.
fn block_line(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 || trimmed.starts_with('\t') {
        None
    } else {
        Some(trimmed)
    }
}

fn opens_fence(line: &str) -> Option<(u8, usize)> {
    let marker = *line.as_bytes().first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = line.bytes().take_while(|b| *b == marker).count();
    if length < 3 || (marker == b'`' && line[length..].contains('`')) {
        return None;
    }
    Some((marker, length))
}

fn closes_fence(line: &str, marker: u8, minimum: usize) -> bool {
    let length = line.bytes().take_while(|b| *b == marker).count();
    length >= minimum && line[length..].trim_matches([' ', '\t']).is_empty()
}

fn heading(line: &str) -> Option<(usize, String)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let tail = &line[level..];
    if !tail.is_empty() && !tail.starts_with([' ', '\t']) {
        return None;
    }
    let mut title = tail.trim_matches([' ', '\t']);
    let without_hashes = title.trim_end_matches('#');
    if without_hashes.is_empty() || without_hashes.ends_with([' ', '\t']) {
        title = without_hashes.trim_end_matches([' ', '\t']);
    }
    Some((level, normalize(title)))
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn checklist_item(line: &str) -> bool {
    let bytes = line.as_bytes();
    let marker_end = if matches!(bytes.first(), Some(b'-' | b'+' | b'*')) {
        1
    } else {
        let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
        if !(1..=9).contains(&digits) || !matches!(bytes.get(digits), Some(b'.' | b')')) {
            return false;
        }
        digits + 1
    };
    let rest = &line[marker_end..];
    if !rest.starts_with([' ', '\t']) {
        return false;
    }
    let rest = rest.trim_start_matches([' ', '\t']);
    let bytes = rest.as_bytes();
    bytes.len() >= 3
        && bytes[0] == b'['
        && matches!(bytes[1], b' ' | b'x' | b'X')
        && bytes[2] == b']'
        && (bytes.len() == 3 || matches!(bytes[3], b' ' | b'\t'))
}

fn separator_cell(cell: &str) -> bool {
    let cell = cell.strip_prefix(':').unwrap_or(cell);
    let cell = cell.strip_suffix(':').unwrap_or(cell);
    cell.len() >= 3 && cell.bytes().all(|b| b == b'-')
}

fn table_cells(line: &str) -> Option<Vec<String>> {
    if !line.contains('|') {
        return None;
    }
    let bytes = line.as_bytes();
    let mut runs = Vec::new();
    let mut index = 0;
    let mut slashes = 0;
    while index < bytes.len() {
        if bytes[index] == b'`' {
            let start = index;
            while index < bytes.len() && bytes[index] == b'`' {
                index += 1;
            }
            runs.push((start, index - start, slashes % 2 == 1));
            slashes = 0;
        } else {
            slashes = if bytes[index] == b'\\' {
                slashes + 1
            } else {
                0
            };
            index += 1;
        }
    }

    // Index matching backtick runs once, so unmatched spans cannot cause
    // repeated scans of the remaining line. Backslashes inside spans are literal.
    let mut next_run = HashMap::new();
    let mut closing = vec![None; runs.len()];
    for (position, &(start, length, escaped)) in runs.iter().enumerate().rev() {
        let opening_length = length - usize::from(escaped);
        if opening_length != 0 {
            closing[position] = next_run.get(&opening_length).copied();
        }
        next_run.insert(length, start + length);
    }

    let mut boundaries = Vec::new();
    let mut run_index = 0;
    index = 0;
    slashes = 0;
    while index < bytes.len() {
        if bytes[index] == b'`' {
            while runs[run_index].0 < index {
                run_index += 1;
            }
            index = closing[run_index].unwrap_or(index + runs[run_index].1);
            run_index += 1;
            slashes = 0;
            continue;
        }
        if bytes[index] == b'|' && slashes % 2 == 0 {
            boundaries.push(index);
        }
        slashes = if bytes[index] == b'\\' {
            slashes + 1
        } else {
            0
        };
        index += 1;
    }
    if boundaries.is_empty() {
        return None;
    }
    let mut cells = Vec::new();
    let mut start = 0;
    for boundary in boundaries {
        cells.push(&line[start..boundary]);
        start = boundary + 1;
    }
    cells.push(&line[start..]);
    if cells.first().is_some_and(|cell| cell.trim().is_empty()) {
        cells.remove(0);
    }
    if cells.last().is_some_and(|cell| cell.trim().is_empty()) {
        cells.pop();
    }
    if cells.is_empty() {
        None
    } else {
        Some(cells.into_iter().map(normalize).collect())
    }
}
