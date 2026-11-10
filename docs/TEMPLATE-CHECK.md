# Explicit Markdown template structure checking

`src/template_check.rs` provides the isolated library function:

```rust
pub fn check(
    template: &str,
    document: &str,
) -> crate::error::Result<serde_json::Value>
```

The caller supplies the full, explicitly selected template and document. The
checker performs no I/O, resource discovery, routing, authorization, persistence,
or metrics collection. Template contents are data and confer no permissions.
The module is exported by the library, CLI and authenticated service. The
console exposes an explicit check from a template's preview.

```sh
# Local mode: no service, configuration or credentials needed.
harness check-template --template-file /path/to/template.md --document-file /path/to/output.md
# Read an authorized server template and check in one readonly request.
harness --namespace shared check-template --template-path memory/templates/release-doc-template.md --document-file /path/to/output.md
```

The CLI prints the report as JSON and exits unsuccessfully when `passed:false`;
invalid input also fails. The server returns HTTP200 for a completed structural
comparison, including `passed:false`. Checking never creates a Session, receipt
or memory write. Remote input travels to the configured trusted server;
local mode keeps it on the machine. Do not provide credentials as document input.

## Selecting the skeleton

If the template contains `<!-- HARNESS-TEMPLATE-START -->` and
`<!-- HARNESS-TEMPLATE-END -->` lines, only that interval is checked.
Alternatively, the first `markdown`/`md` fenced block inside an ATX section whose
title ends with `模板结构` is the skeleton. This supports existing guide-plus-template
resources, including four-backtick outer fences around inner JSON/SQL examples.
Unclosed explicit skeletons fail. Without these conventions the whole input is
the template; unrelated code fences remain examples and are ignored.
The report includes `template_source`; missing line numbers refer to the original
template. Put markers around actual Markdown blocks, not a fenced example.

Each input has an independent maximum of **256 KiB (262,144 UTF-8 bytes)**,
inclusive. NUL is rejected in either input. Validation returns the existing
`Error::Invalid`; content is never truncated. An empty document is permitted and
reports missing requirements. A template with no supported heading, table
header, or checklist item is rejected, including an empty or prose-only
template, so unsupported structure cannot produce a vacuous pass.

## Matching rules

- Every supported ATX heading is required, including the title. Headings must
  match their numeric level, normalized text, order among siblings, and actual
  parent. Extra sibling headings are allowed. Extra intermediate parent
  headings change hierarchy and therefore do not satisfy the requirement.
  Skipped levels are allowed when both inputs have the same resulting parent.
- In template heading text, `{nonempty placeholder}` matches a nonempty
  replacement. Literal text around placeholders must still match; other heading
  text is exact. At most 64 headings may contain placeholders. Placeholders in
  body prose are unrestricted; this is not a variable substitution engine.
- Heading matches consume distinct occurrences, greedily from left to right
  under each matched parent. Duplicate names cannot share a match or borrow a
  heading from another parent. There is no backtracking to select a later
  duplicate based on its section contents.
- Tables and checklists belong to the immediate section between a heading and
  the next heading, including child headings. Content before the first heading
  belongs to a separate preamble. Requirements cannot move into another
  section or a subsection.
- Each required table needs its own header plus a valid delimiter row, in the
  same section and in table order. Header column count, column order, and
  normalized cell text must match. Extra tables are allowed. Alignment,
  optional outer pipes, body row counts, and body cell values are not checked.
- Each section needs at least as many checklist items as its template section.
  `[ ]`, `[x]`, and `[X]` all count; labels can change and extra items are
  allowed. Checklist positions relative to prose, tables, or other checklist
  groups are not compared. Plain bullets cannot replace checklist items.
- Normalization trims and collapses whitespace in heading titles and header
  cells. Matching is case sensitive, preserves inline Markdown source, and
  does not render or equate different emphasis, links, entities, or escapes.
  Filled body prose is unrestricted.

## Supported syntax and limits

This is a bounded recognizer, **not a full Markdown parser** or a CommonMark/GFM
conformance check.

Supported standalone blocks are indented by zero to three ASCII spaces.
Four-space or tab-indented lines are ignored; block quotes and container
Markdown are not unwrapped. Nested list structure is not modeled. A checklist
line with up to three leading spaces can count, but deeper list indentation
does not. Setext headings, HTML structure, YAML metadata, and directives are
not interpreted. Visible supported syntax inside unmodeled constructs is still
scanned as ordinary lines; callers must not rely on full Markdown container,
HTML comment, front matter, or multi-line inline-code interpretation.

ATX headings use one through six `#` characters followed by ASCII space, tab,
or end of line. Optional closing hashes are removed only when separated from
the title by whitespace. CRLF and LF are both supported with one-based physical
line numbers.

Backtick and tilde fenced code blocks use at least three matching characters.
Closers require the same marker, at least the opening length, and only spaces
or tabs afterward. Backtick fence info strings cannot contain backticks.
An unclosed fence suppresses the rest of that input. Fences and all their
contents contribute no headings, tables, or checklist items. Four-space
indented lines cannot open or close fences.

A table requires a header containing at least one unescaped pipe outside a
paired backtick span, immediately followed by a delimiter row with the same
number of cells. Each delimiter cell contains at least three hyphens and may
have a colon at either end. Cells can be empty. Optional leading/trailing
pipes are accepted. Backslash parity determines whether a pipe is escaped;
paired equal-length backtick runs protect internal pipes, including spans
containing different-length runs. Unmatched backticks are literal and do not
hide subsequent pipes. This protection is a deliberate recognition rule and
does not claim exact table behavior in every Markdown renderer. Inline spans
are confined to one physical row.

After a table header, subsequent pipe-containing body rows are ignored until
a non-row line, heading, or fence. Use a blank line between distinct tables.
Table body cells cannot create checklist requirements. Single-column tables
must use an outer pipe in both the header and delimiter.

Checklists require an unordered `-`, `+`, or `*` marker, or an ordered marker
with one to nine digits followed by `.` or `)`, then whitespace and a checkbox.
The checkbox must be followed by whitespace or end of line. Checkbox labels,
checked state, indentation hierarchy, and completion are not validated.

Parsing and matching use bounded input scans and indexes; exact repeated missing
or duplicate headings/tables do not trigger repeated whole-document searches.
Placeholder headings use bounded scans with the separate 64-heading limit.
No recursive traversal proportional to the number of sections is used.

## Result

Structural differences are successful library calls with `passed: false`.
For example, a matching heading with two required checkboxes and only one
actual checkbox returns:

```json
{
  "check": "markdown_template_structure",
  "version": 1,
  "passed": false,
  "structural_only": true,
  "semantic_guarantee": false,
  "summary": {
    "headings": { "required": 1, "matched": 1 },
    "tables": { "required": 0, "matched": 0 },
    "checklist_items": { "required": 2, "matched": 1 }
  },
  "missing": [
    {
      "kind": "checklist",
      "template_line": 2,
      "section_template_line": 1,
      "required": 2,
      "found": 1,
      "missing": 1
    }
  ]
}
```

`passed` is true exactly when `missing` is empty. Summary matches never exceed
requirements; additional document structure does not inflate them. Missing
entries use template locations, so duplicate headings are distinguishable:

| `kind` | Additional fields |
| --- | --- |
| `heading` | `template_line`, `parent_template_line`, `level`, `text` |
| `table_header` | `template_line`, `section_template_line`, `headers` |
| `checklist` | First item `template_line`, `section_template_line`, `required`, `found`, `missing` |

Parent/section line `0` denotes the preamble. A missing parent also causes its
required descendants, tables, and checklist counts to be reported missing.
Entries follow template section order, with the heading first, then missing
tables, then a checklist count deficit. They do not diagnose exact document
locations or distinguish absent structure from a wrong order, level, or parent.
Reports can contain template titles and header labels; callers should apply
the same access controls as for the selected template.

**A passing result guarantees only the supported structural comparison.**
It cannot guarantee factual accuracy, adequate prose, correct table values,
task completion, semantic equivalence, or that the document is publishable.
Unsupported Markdown may require manual review even after a pass.

## Isolated tests

`tests/template_check.rs` exercises the exported library API:

```sh
cargo test --locked --test template_check
cargo test --locked --no-default-features --test template_check
```

Tests use synthetic inline strings only, without private memories or fixtures.
