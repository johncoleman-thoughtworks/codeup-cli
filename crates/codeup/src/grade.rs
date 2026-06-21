use codeup_core::schema::{Finding, Severity, Status};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grade {
    A,
    B,
    C,
    D,
}

impl Grade {
    pub fn as_str(self) -> &'static str {
        match self {
            Grade::A => "A",
            Grade::B => "B",
            Grade::C => "C",
            Grade::D => "D",
        }
    }

    fn emoji(self) -> &'static str {
        match self {
            Grade::A => "✅",
            Grade::B => "🟡",
            Grade::C => "🟠",
            Grade::D => "❌",
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct StatusCounts {
    pub unconfirmed: usize,
    pub confirmed: usize,
    pub dismissed: usize,
    pub fixed: usize,
}

impl StatusCounts {
    fn active(self) -> usize {
        self.unconfirmed + self.confirmed
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SeverityStatusCounts {
    pub high: StatusCounts,
    pub medium: StatusCounts,
    pub low: StatusCounts,
}

impl SeverityStatusCounts {
    fn tally(&mut self, severity: Severity, status: Status) {
        let bucket = match severity {
            Severity::High => &mut self.high,
            Severity::Medium => &mut self.medium,
            Severity::Low => &mut self.low,
        };
        match status {
            Status::Unconfirmed => bucket.unconfirmed += 1,
            Status::Confirmed => bucket.confirmed += 1,
            Status::Dismissed => bucket.dismissed += 1,
            Status::Fixed => bucket.fixed += 1,
        }
    }
}

pub struct ScanMeta {
    pub scanned_at: String,
    pub file_count: usize,
    pub provider_label: Option<String>,
}

pub struct GradeSummary {
    pub grade: Grade,
    pub counts: SeverityStatusCounts,
    pub scan_meta: ScanMeta,
}

pub fn compute(findings: &[Finding], meta: ScanMeta) -> GradeSummary {
    let mut counts = SeverityStatusCounts::default();
    for f in findings {
        counts.tally(f.severity, f.status);
    }

    let grade = if counts.high.active() > 0 {
        Grade::D
    } else if counts.medium.active() > 0 {
        Grade::C
    } else if counts.low.active() > 0 {
        Grade::B
    } else {
        Grade::A
    };

    GradeSummary { grade, counts, scan_meta: meta }
}

pub fn render_markdown(summary: &GradeSummary) -> String {
    let g = summary.grade;
    let c = &summary.counts;
    let m = &summary.scan_meta;

    let mut out = String::new();

    out.push_str(&format!(
        "## Codeup quality grade: {} {}\n\n",
        g.as_str(),
        g.emoji()
    ));

    if let Some(advice) = grade_advice(g, c) {
        out.push_str(&format!("> {advice}\n\n"));
    }

    out.push_str("| Severity | Unconfirmed | Confirmed | Dismissed | Fixed |\n");
    out.push_str("|----------|-------------|-----------|-----------|-------|\n");
    for (label, row) in [("high", c.high), ("medium", c.medium), ("low", c.low)] {
        out.push_str(&format!(
            "| {label:<8} | {:<11} | {:<9} | {:<9} | {:<5} |\n",
            row.unconfirmed, row.confirmed, row.dismissed, row.fixed
        ));
    }

    let provider = m
        .provider_label
        .as_deref()
        .unwrap_or("deterministic-only");
    out.push_str(&format!(
        "\n_Scan: {} · {} files · provider: {}_\n",
        m.scanned_at, m.file_count, provider
    ));

    out
}

fn grade_advice(grade: Grade, c: &SeverityStatusCounts) -> Option<String> {
    match grade {
        Grade::A => None,
        Grade::B => {
            let n = c.low.active();
            Some(format!(
                "**{n} active low-severity finding{}** — no blockers, but worth reviewing.",
                if n == 1 { "" } else { "s" }
            ))
        }
        Grade::C => {
            let n = c.medium.active();
            Some(format!(
                "**{n} active medium-severity finding{}** require attention before this can reach A.",
                if n == 1 { "" } else { "s" }
            ))
        }
        Grade::D => {
            let n = c.high.active();
            Some(format!(
                "**{n} active high-severity finding{}** must be resolved before this can reach C.",
                if n == 1 { "" } else { "s" }
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeup_core::schema::{FindingLocation, Priority};

    fn finding(severity: Severity, status: Status) -> Finding {
        Finding {
            schema_version: 1,
            id: "test-id".into(),
            category: "test-category".into(),
            severity,
            status,
            priority: Priority::Medium,
            location: FindingLocation {
                file: "src/foo.rs".into(),
                line: Some(1),
                end_line: None,
                ast_path: None,
                content_hash: None,
            },
            explanation: "test".into(),
            suggested_remediation: None,
            detected_at: "2026-06-21T00:00:00.000Z".into(),
            detected_by: "test".into(),
            confidence: None,
            history: vec![],
        }
    }

    fn meta() -> ScanMeta {
        ScanMeta {
            scanned_at: "2026-06-21T14:03:00.000Z".into(),
            file_count: 42,
            provider_label: Some("anthropic/claude-sonnet-4-6".into()),
        }
    }

    // --- grade computation ---

    #[test]
    fn grade_a_no_findings() {
        let summary = compute(&[], meta());
        assert_eq!(summary.grade, Grade::A);
    }

    #[test]
    fn grade_a_all_dismissed_or_fixed() {
        let findings = vec![
            finding(Severity::High, Status::Dismissed),
            finding(Severity::High, Status::Fixed),
            finding(Severity::Medium, Status::Fixed),
            finding(Severity::Low, Status::Dismissed),
        ];
        let summary = compute(&findings, meta());
        assert_eq!(summary.grade, Grade::A);
    }

    #[test]
    fn grade_b_only_low_active() {
        let findings = vec![
            finding(Severity::Low, Status::Unconfirmed),
            finding(Severity::Low, Status::Confirmed),
            finding(Severity::High, Status::Fixed),
        ];
        let summary = compute(&findings, meta());
        assert_eq!(summary.grade, Grade::B);
    }

    #[test]
    fn grade_c_medium_active_no_high() {
        let findings = vec![
            finding(Severity::Medium, Status::Unconfirmed),
            finding(Severity::Low, Status::Confirmed),
            finding(Severity::High, Status::Dismissed),
        ];
        let summary = compute(&findings, meta());
        assert_eq!(summary.grade, Grade::C);
    }

    #[test]
    fn grade_d_any_high_active_unconfirmed() {
        let findings = vec![finding(Severity::High, Status::Unconfirmed)];
        let summary = compute(&findings, meta());
        assert_eq!(summary.grade, Grade::D);
    }

    #[test]
    fn grade_d_any_high_active_confirmed() {
        let findings = vec![finding(Severity::High, Status::Confirmed)];
        let summary = compute(&findings, meta());
        assert_eq!(summary.grade, Grade::D);
    }

    #[test]
    fn grade_d_wins_over_lower_severity() {
        let findings = vec![
            finding(Severity::High, Status::Confirmed),
            finding(Severity::Medium, Status::Confirmed),
            finding(Severity::Low, Status::Unconfirmed),
        ];
        let summary = compute(&findings, meta());
        assert_eq!(summary.grade, Grade::D);
    }

    // --- count tallying ---

    #[test]
    fn counts_split_correctly_by_status() {
        let findings = vec![
            finding(Severity::High, Status::Unconfirmed),
            finding(Severity::High, Status::Confirmed),
            finding(Severity::High, Status::Dismissed),
            finding(Severity::High, Status::Fixed),
            finding(Severity::Medium, Status::Unconfirmed),
            finding(Severity::Low, Status::Fixed),
        ];
        let summary = compute(&findings, meta());
        let c = summary.counts;
        assert_eq!(c.high.unconfirmed, 1);
        assert_eq!(c.high.confirmed, 1);
        assert_eq!(c.high.dismissed, 1);
        assert_eq!(c.high.fixed, 1);
        assert_eq!(c.medium.unconfirmed, 1);
        assert_eq!(c.medium.confirmed, 0);
        assert_eq!(c.low.fixed, 1);
    }

    // --- markdown rendering ---

    #[test]
    fn render_grade_a_has_no_advice_block() {
        let summary = compute(&[], meta());
        let md = render_markdown(&summary);
        assert!(md.contains("## Codeup quality grade: A ✅"));
        assert!(!md.contains('>'), "grade A should have no blockquote advice");
        assert!(md.contains("| high"));
        assert!(md.contains("anthropic/claude-sonnet-4-6"));
        assert!(md.contains("42 files"));
    }

    #[test]
    fn render_grade_b_includes_low_count_advice() {
        let findings = vec![
            finding(Severity::Low, Status::Unconfirmed),
            finding(Severity::Low, Status::Unconfirmed),
        ];
        let summary = compute(&findings, meta());
        let md = render_markdown(&summary);
        assert!(md.contains("## Codeup quality grade: B 🟡"));
        assert!(md.contains("**2 active low-severity findings**"));
    }

    #[test]
    fn render_grade_c_includes_medium_count_advice() {
        let findings = vec![finding(Severity::Medium, Status::Confirmed)];
        let summary = compute(&findings, meta());
        let md = render_markdown(&summary);
        assert!(md.contains("## Codeup quality grade: C 🟠"));
        assert!(md.contains("**1 active medium-severity finding**"));
        assert!(!md.contains("findings**"), "singular form should be used");
    }

    #[test]
    fn render_grade_d_includes_high_count_advice() {
        let findings = vec![
            finding(Severity::High, Status::Unconfirmed),
            finding(Severity::High, Status::Confirmed),
        ];
        let summary = compute(&findings, meta());
        let md = render_markdown(&summary);
        assert!(md.contains("## Codeup quality grade: D ❌"));
        assert!(md.contains("**2 active high-severity findings**"));
    }

    #[test]
    fn render_deterministic_only_label_when_no_provider() {
        let no_provider_meta = ScanMeta {
            scanned_at: "2026-06-21T00:00:00.000Z".into(),
            file_count: 5,
            provider_label: None,
        };
        let summary = compute(&[], no_provider_meta);
        let md = render_markdown(&summary);
        assert!(md.contains("deterministic-only"));
    }

    #[test]
    fn render_contains_all_three_severity_rows() {
        let summary = compute(&[], meta());
        let md = render_markdown(&summary);
        assert!(md.contains("| high"));
        assert!(md.contains("| medium"));
        assert!(md.contains("| low"));
    }

    #[test]
    fn render_table_counts_match_findings() {
        let findings = vec![
            finding(Severity::High, Status::Dismissed),
            finding(Severity::Medium, Status::Unconfirmed),
            finding(Severity::Medium, Status::Confirmed),
        ];
        let summary = compute(&findings, meta());
        let md = render_markdown(&summary);
        // Medium row: 1 unconfirmed, 1 confirmed, 0 dismissed, 0 fixed
        assert!(md.contains("| medium   | 1           | 1         | 0         | 0     |"));
        // High row: 0 unconfirmed, 0 confirmed, 1 dismissed, 0 fixed
        assert!(md.contains("| high     | 0           | 0         | 1         | 0     |"));
    }
}