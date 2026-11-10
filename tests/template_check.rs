use harness_server::error;
use harness_server::template_check::check;
use serde_json::Value;

#[test]
fn guide_extracts_only_markdown_skeleton_and_fills_heading_placeholders() {
    let template = "# Guide\n## 使用约定\nNot generated.\n## 二、模板结构\n````markdown\n# {名称} 发布\n## 计划\n| Owner | Stage |\n| --- | --- |\n```sql\n# not heading\n```\n- [ ] Confirm\n````\n## 快速检查\nNot generated.";
    let report = check(
        template,
        "# Billing 发布\n## 计划\n| Owner | Stage |\n| --- | --- |\n- [x] Done",
    )
    .unwrap();
    assert_eq!(report["passed"], true, "{report}");
    assert_eq!(report["template_source"], "markdown_skeleton");
    assert_eq!(report["summary"]["headings"]["required"], 2);
    let missing = check(template, "# Billing 发布").unwrap();
    assert_eq!(missing["missing"][0]["template_line"], 7);
}

#[test]
fn explicit_markers_and_unclosed_skeletons_have_clear_results() {
    let template = "# Guide\n<!-- HARNESS-TEMPLATE-START -->\n# {功能}\n## API\n<!-- HARNESS-TEMPLATE-END -->\n# Notes";
    assert_eq!(check(template, "# Orders\n## API").unwrap()["passed"], true);
    assert!(check("## 模板结构\n```markdown\n# Missing fence", "").is_err());
    assert!(check("<!-- HARNESS-TEMPLATE-START -->\n# Missing marker", "").is_err());
    assert_eq!(
        check("# Literal title", "# Other").unwrap()["passed"],
        false
    );
}

fn missing(report: &Value) -> &[Value] {
    report["missing"].as_array().unwrap()
}

#[test]
fn filled_document_preserves_structure_without_copying_placeholder_prose() {
    let template = "# Plan\nPlaceholder instructions.\n\n## Rollout\n\
        | Stage | Owner |\n| --- | --- |\n| TBD | TBD |\n\n\
        - [ ] Placeholder one\n- [ ] Placeholder two\n";
    let document = "# Plan\nDeploy the new service on Friday.\n\n## Rollout\n\
        Stage | Owner\n:--- | ---:\nProduction | Release team\n\
        Canary | Operations\n\n- [x] Review completed\n* [X] Rollback tested\n\
        + [ ] Optional follow-up\n\nActual conclusions.\n";
    let report = check(template, document).unwrap();
    assert_eq!(report["passed"], true, "{report}");
    assert_eq!(report["structural_only"], true);
    assert_eq!(report["semantic_guarantee"], false);
    assert_eq!(report["summary"]["headings"]["matched"], 2);
    assert_eq!(report["summary"]["tables"]["matched"], 1);
    assert_eq!(report["summary"]["checklist_items"]["matched"], 2);
    assert!(missing(&report).is_empty());
}

#[test]
fn reports_each_missing_kind_with_template_locations_and_counts() {
    let template = "# Plan\n## Rollout\n\
        | Stage | Owner |\n| --- | --- |\n\n- [ ] First\n- [ ] Second\n";
    let report = check(template, "# Plan\n## Rollout\n- [x] Done\n").unwrap();
    assert_eq!(report["passed"], false);
    assert_eq!(
        missing(&report),
        &[
            serde_json::json!({
                "kind": "table_header", "template_line": 3,
                "section_template_line": 2, "headers": ["Stage", "Owner"]
            }),
            serde_json::json!({
                "kind": "checklist", "template_line": 6,
                "section_template_line": 2, "required": 2, "found": 1, "missing": 1
            }),
        ]
    );
}

#[test]
fn headings_must_match_in_order_and_case() {
    let report = check("# First\n# Second\n", "# Second\n# First\n").unwrap();
    assert_eq!(report["passed"], false);
    assert_eq!(missing(&report)[0]["text"], "Second");
    assert_eq!(check("# Plan", "# plan").unwrap()["passed"], false);
}

#[test]
fn heading_level_and_actual_parent_must_match() {
    let template = "# Plan\n## Rollout\n### Risks\n";
    for document in [
        "# Plan\n### Rollout\n### Risks\n",
        "# Plan\n## Other\n### Risks\n## Rollout\n",
        "# Plan\n## Rollout\n## Other\n### Risks\n",
        "# Wrapper\n## Plan\n### Rollout\n#### Risks\n",
    ] {
        assert_eq!(
            check(template, document).unwrap()["passed"],
            false,
            "{document}"
        );
    }
}

#[test]
fn allows_additional_sibling_headings_and_skipped_heading_levels() {
    let template = "# Plan\n### Scope\n### Rollout\n";
    let document = "# Extra\n# Plan\n### Intro\n### Scope\n\
        #### Detail\n### Other\n### Rollout\n# Appendix\n";
    assert_eq!(check(template, document).unwrap()["passed"], true);
}

#[test]
fn duplicate_headings_use_distinct_ordered_occurrences() {
    let template = "# Plan\n## Review\n- [ ] One\n## Review\n- [ ] Two\n";
    let report = check(template, "# Plan\n## Review\n- [x] One\n- [x] Two\n").unwrap();
    assert_eq!(report["passed"], false);
    assert_eq!(missing(&report)[0]["kind"], "heading");
    assert_eq!(missing(&report)[0]["template_line"], 4);
    assert_eq!(missing(&report)[1]["found"], 0);
    assert_eq!(
        check(
            template,
            "# Plan\n## Review\n- [x] First\n## Review\n- [X] Second\n"
        )
        .unwrap()["passed"],
        true
    );
}

#[test]
fn identical_child_names_in_other_parents_cannot_satisfy_requirements() {
    let template = "# A\n## Review\n# B\n## Review\n";
    let report = check(template, "# A\n## Review\n## Review\n# B\n").unwrap();
    assert_eq!(report["passed"], false);
    assert_eq!(missing(&report)[0]["template_line"], 4);
    assert_eq!(missing(&report)[0]["parent_template_line"], 3);
}

#[test]
fn missing_parent_reports_its_entire_required_subtree() {
    let template = "# A\n## B\n| Key |\n| --- |\n- [ ] Check\n";
    let report = check(template, "").unwrap();
    assert_eq!(missing(&report).len(), 4);
    assert_eq!(report["summary"]["headings"]["required"], 2);
    assert_eq!(report["summary"]["headings"]["matched"], 0);
}

#[test]
fn table_and_checklist_requirements_cannot_move_between_sections() {
    let template = "# Plan\n## Scope\n| Key |\n| --- |\n\n- [ ] Check\n## Rollout\n";
    let document = "# Plan\n## Scope\n## Rollout\n| Key |\n| --- |\n\n- [x] Done\n";
    let report = check(template, document).unwrap();
    assert_eq!(report["passed"], false);
    assert_eq!(missing(&report).len(), 2);
    let child = "# Plan\n## Scope\n### Child\n| Key |\n| --- |\n\n- [x] Done\n## Rollout\n";
    assert_eq!(check(template, child).unwrap()["passed"], false);
}

#[test]
fn preamble_structures_have_their_own_scope() {
    let template = "| Key |\n| --- |\n\n- [ ] Before\n# Plan\n";
    let report = check(template, "# Plan\n| Key |\n| --- |\n\n- [x] After\n").unwrap();
    assert_eq!(report["passed"], false);
    assert_eq!(missing(&report)[0]["section_template_line"], 0);
    assert_eq!(missing(&report)[1]["section_template_line"], 0);
    assert_eq!(
        check(template, "| Key |\n| --- |\n\n- [x] Before\n# Plan\n").unwrap()["passed"],
        true
    );
}

#[test]
fn table_headers_require_valid_delimiters_and_column_order() {
    let template = "| Name | Owner |\n| --- | --- |\n";
    for document in [
        "| Name | Owner |\n",
        "| Name | Owner |\n| --- |\n",
        "| Name | Owner |\n| -- | --- |\n",
        "| Owner | Name |\n| --- | --- |\n",
        "| Name | Assignee |\n| --- | --- |\n",
        "| Name | Owner | Extra |\n| --- | --- | --- |\n",
    ] {
        assert_eq!(
            check(template, document).unwrap()["passed"],
            false,
            "{document}"
        );
    }
}

#[test]
fn repeated_tables_are_consumed_once_and_in_order() {
    let a = "| A |\n| --- |\n\n";
    let b = "| B |\n| --- |\n\n";
    assert_eq!(check(&format!("{a}{a}"), a).unwrap()["passed"], false);
    assert_eq!(
        check(&format!("{a}{b}"), &format!("{b}{a}")).unwrap()["passed"],
        false
    );
    assert_eq!(
        check(&format!("{a}{a}"), &format!("{b}{a}{a}{b}")).unwrap()["passed"],
        true
    );
}

#[test]
fn table_cells_preserve_escaped_and_code_span_pipes() {
    let template = r"| Name \| alias | `a|b` | ``a`|b`` |
| --- | --- | --- |
";
    let document = r"Name \| alias | `a|b` | ``a`|b``
:--- | ---: | :---:
actual | data | values
";
    assert_eq!(check(template, document).unwrap()["passed"], true);
    let report = check(template, "").unwrap();
    assert_eq!(
        missing(&report)[0]["headers"],
        serde_json::json!([r"Name \| alias", "`a|b`", "``a`|b``"])
    );
}

#[test]
fn escaped_backslashes_and_unmatched_backticks_do_not_hide_delimiters() {
    for (header, columns) in [
        (r"| A \\| B |", vec![r"A \\", "B"]),
        (r"| A \\\| B | C |", vec![r"A \\\| B", "C"]),
        ("| `A | B |", vec!["`A", "B"]),
        (r"| \`A | B |", vec![r"\`A", "B"]),
        (r"| `A\` | B |", vec![r"`A\`", "B"]),
    ] {
        let template = format!("{header}\n| --- | --- |\n");
        let report = check(&template, "").unwrap();
        assert_eq!(
            missing(&report)[0]["headers"],
            serde_json::json!(columns),
            "{header}"
        );
    }
}

#[test]
fn differently_sized_backtick_runs_do_not_close_a_span() {
    let template = "| ``A ` | B`` | C |\n| --- | --- |\n";
    let report = check(template, "").unwrap();
    assert_eq!(
        missing(&report)[0]["headers"],
        serde_json::json!(["``A ` | B``", "C"])
    );
}

#[test]
fn checklist_counts_accept_ordered_unordered_checked_and_empty_items() {
    let template = "- [ ] One\n+ [ ] Two\n* [ ] Three\n1. [ ] Four\n2) [ ] Five\n";
    let document = "- [x] Changed\n+ [X] Changed\n* [ ]\n23. [X] Done\n9) [x]\n";
    assert_eq!(check(template, document).unwrap()["passed"], true);
}

#[test]
fn plain_bullets_and_checkbox_like_prose_are_not_checklist_items() {
    let template = "# Plan\n- [ ] Required\n";
    for document in [
        "# Plan\n- Ordinary list item\n",
        "# Plan\n[x] Standalone\n",
        "# Plan\n- [x]without whitespace\n",
        "# Plan\n- [y] Invalid state\n",
        "# Plan\n-[x] No marker whitespace\n",
        "# Plan\n1000000000. [x] Too many digits\n",
        "# Plan\nProse - [x] example\n",
    ] {
        assert_eq!(check(template, document).unwrap()["passed"], false);
    }
}

#[test]
fn table_body_cells_do_not_add_checklist_requirements() {
    let template = "| State | Note |\n| --- | --- |\n- [ ] Example | Ignore\n\n- [ ] Required\n";
    let report = check(
        template,
        "| State | Note |\n| --- | --- |\n\n- [x] Actual\n",
    )
    .unwrap();
    assert_eq!(report["passed"], true);
    assert_eq!(report["summary"]["checklist_items"]["required"], 1);
}

#[test]
fn fenced_examples_are_ignored_in_template_and_document() {
    let template = "# Plan\n```md\n## Fake\n| Fake |\n| --- |\n- [ ] Fake\n```\n\
        ~~~\n## Also fake\n~~~\n## Real\n- [ ] Check\n";
    assert_eq!(
        check(template, "# Plan\n## Real\n- [x] Done\n").unwrap()["passed"],
        true
    );
    let report = check(
        "# Plan\n## Real\n- [ ] Check\n",
        "# Plan\n```md\n## Real\n- [x] Done\n```\n",
    )
    .unwrap();
    assert_eq!(report["passed"], false);
    assert_eq!(missing(&report).len(), 2);
}

#[test]
fn fences_require_matching_marker_minimum_length_and_empty_closing_tail() {
    let template = "# Plan\n## Real\n";
    for document in [
        "# Plan\n````md\n```\n## Real\n",
        "# Plan\n```md\n~~~\n## Real\n",
        "# Plan\n```md\n``` still code\n## Real\n",
        "# Plan\n~~~md\n~~\n## Real\n",
        "# Plan\n   ```md\n    ```\n## Real\n",
    ] {
        assert_eq!(
            check(template, document).unwrap()["passed"],
            false,
            "{document}"
        );
    }
    assert_eq!(
        check(
            template,
            "# Plan\n   ```md\n#### Example\n  ```` \t\n## Real\n"
        )
        .unwrap()["passed"],
        true
    );
}

#[test]
fn invalid_backtick_info_string_is_not_a_fence() {
    assert_eq!(
        check("# Plan\n## Real\n", "# Plan\n```bad`info\n## Real\n").unwrap()["passed"],
        true
    );
}

#[test]
fn crlf_heading_spacing_and_optional_closing_hashes_are_normalized() {
    let template = " #  Plan\t details ###\r\n\r\n  ## Scope ##\r\n\
        | Key | Owner |\r\n| --- | --- |\r\n\r\n  - [ ] Check\r\n";
    let document = "# Plan details\n## Scope\nKey | Owner\n--- | ---\n\n- [X] Changed\n";
    let report = check(template, document).unwrap();
    assert_eq!(report["passed"], true, "{report}");
    assert_eq!(check("# C#", "# C# ###").unwrap()["passed"], true);
    assert_eq!(check("#", "# ###").unwrap()["passed"], true);
}

#[test]
fn ignores_indented_code_quoted_blocks_and_unsupported_heading_syntax() {
    let template = "# Real\n";
    for document in [
        "    # Real\n",
        "\t# Real\n",
        "> # Real\n",
        "Real\n====\n",
        "#Real\n",
        "####### Real\n",
        "\\# Real\n",
    ] {
        assert_eq!(check(template, document).unwrap()["passed"], false);
    }
    let report = check(
        "# Real\n    ## Code\n\t- [ ] Code\n> ## Quoted\n\
         Setext\n---\n\\# Escaped\n####### Too deep\n",
        "# Real\n",
    )
    .unwrap();
    assert_eq!(report["passed"], true);
    assert_eq!(report["summary"]["headings"]["required"], 1);
}

#[test]
fn supports_unicode_without_changing_title_case_or_inline_markup() {
    let template = "# 发布计划\n## **风险**\n| 阶段 | 负责人 |\n| --- | --- |\n\n- [ ] 确认\n";
    assert_eq!(
        check(
            template,
            "# 发布计划\n## **风险**\n| 阶段 | 负责人 |\n| --- | --- |\n\n- [x] 已核验\n"
        )
        .unwrap()["passed"],
        true
    );
    assert_eq!(check("# **Plan**", "# Plan").unwrap()["passed"], false);
}

#[test]
fn rejects_nul_and_oversized_inputs_independently() {
    let oversized = "x".repeat(256 * 1024 + 1);
    for (template, document) in [
        ("# Plan\0", "# Plan"),
        ("# Plan", "# Plan\0"),
        (oversized.as_str(), "# Plan"),
        ("# Plan", oversized.as_str()),
    ] {
        let error = check(template, document).unwrap_err();
        assert!(matches!(error, error::Error::Invalid(_)));
    }
    let unicode_bytes = format!("# Plan\n{}", "界".repeat(90_000));
    assert!(check("# Plan", &unicode_bytes).is_err());
}

#[test]
fn accepts_inputs_exactly_at_the_byte_limit() {
    let input = format!("# Plan\n{}", "x".repeat(256 * 1024 - 7));
    assert_eq!(input.len(), 256 * 1024);
    assert_eq!(check(&input, &input).unwrap()["passed"], true);
}

#[test]
fn rejects_vacuous_templates_but_allows_an_empty_document() {
    for template in [
        "",
        " \r\n\t",
        "Instructions without supported structure.",
        "```\n# Example\n- [ ] Example\n```\n",
        "Setext\n===\n",
    ] {
        assert!(check(template, "# Anything").is_err(), "{template:?}");
    }
    assert_eq!(check("# Plan", "").unwrap()["passed"], false);
}

#[test]
fn large_unmatched_heading_sets_and_deeply_repeated_sections_are_bounded() {
    let template = (0..4000)
        .map(|index| format!("# Required {index}\n"))
        .collect::<String>();
    let document = (0..4000)
        .map(|index| format!("# Actual {index}\n"))
        .collect::<String>();
    let report = check(&template, &document).unwrap();
    assert_eq!(missing(&report).len(), 4000);
    let repeated = "# A\n## A\n### A\n#### A\n##### A\n###### A\n".repeat(1000);
    assert_eq!(check(&repeated, &repeated).unwrap()["passed"], true);
}

#[test]
fn generated_mixed_syntax_is_safe_and_self_consistent() {
    let alphabet = [
        '#', '|', '`', '~', '\\', '-', ':', '[', ']', 'x', '.', ')', '1', '*', '+', ' ', '\t',
        '\r', 'A', '界',
    ];
    let mut state = 19_u64;
    for _ in 0..200 {
        let mut input = String::from("# Plan\n");
        for _ in 0..40 {
            for _ in 0..60 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                input.push(alphabet[(state >> 32) as usize % alphabet.len()]);
            }
            input.push('\n');
        }
        let report = check(&input, &input).unwrap();
        assert_eq!(report["passed"], true, "{report}");
    }
}

#[test]
fn many_unmatched_backtick_runs_and_missing_tables_do_not_rescan_suffixes() {
    let mut header = String::from("| ");
    for length in 1..=680 {
        header.push_str(&"`".repeat(length));
        header.push(' ');
    }
    header.push_str("| Owner |");
    let template = format!("{header}\n| --- | --- |\n");
    let report = check(&template, "").unwrap();
    assert_eq!(report["summary"]["tables"]["required"], 1);
    assert_eq!(report["passed"], false);

    let template = (0..2500)
        .map(|index| format!("| Required {index} |\n| --- |\n\n"))
        .collect::<String>();
    let document = (0..2500)
        .map(|index| format!("| Actual {index} |\n| --- |\n\n"))
        .collect::<String>();
    let report = check(&template, &document).unwrap();
    assert_eq!(missing(&report).len(), 2500);
    assert_eq!(report["summary"]["tables"]["matched"], 0);
}
