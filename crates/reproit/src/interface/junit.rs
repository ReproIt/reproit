//! JUnit XML output so reproit results slot into any CI (GitLab, GitHub,
//! Jenkins all consume JUnit). One testcase per gate run / journey; a
//! failure element with the evidence dir on FAIL.

use std::path::Path;

pub struct Case {
    pub name: String,
    pub passed: bool,
    pub time_s: f64,
    pub message: String,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn write(path: &Path, suite: &str, cases: &[Case]) -> std::io::Result<()> {
    let failures = cases.iter().filter(|c| !c.passed).count();
    let total_time: f64 = cases.iter().map(|c| c.time_s).sum();
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" time=\"{:.1}\">\n",
        esc(suite),
        cases.len(),
        failures,
        total_time
    ));
    for c in cases {
        xml.push_str(&format!(
            "  <testcase name=\"{}\" classname=\"reproit.{}\" time=\"{:.1}\"",
            esc(&c.name),
            esc(suite),
            c.time_s
        ));
        if c.passed {
            xml.push_str("/>\n");
        } else {
            xml.push_str(&format!(
                ">\n    <failure message=\"{}\"/>\n  </testcase>\n",
                esc(&c.message)
            ));
        }
    }
    xml.push_str("</testsuite>\n");
    std::fs::write(path, xml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_shape_and_escaping() {
        let cases = vec![
            Case {
                name: "run 1".into(),
                passed: true,
                time_s: 1.0,
                message: String::new(),
            },
            Case {
                name: "run <2> & \"x\"".into(),
                passed: false,
                time_s: 2.0,
                message: "boom".into(),
            },
        ];
        let dir = std::env::temp_dir().join("reproit_junit_test.xml");
        write(&dir, "gate.demo", &cases).unwrap();
        let xml = std::fs::read_to_string(&dir).unwrap();
        assert!(xml.contains("tests=\"2\" failures=\"1\""));
        assert!(xml.contains("run &lt;2&gt; &amp; &quot;x&quot;"));
        assert!(xml.contains("<failure message=\"boom\"/>"));
    }
}
