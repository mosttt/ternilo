use codepage_strings::Coding;

fn decode(bytes: &[u8], code_page: u16) -> Result<String, codepage_strings::ConvertError> {
    // BOMs and explicit UTF-8 output take precedence over the host's legacy code page.
    for (bom, page) in [(&b"\xff\xfe"[..], 1200), (&b"\xfe\xff"[..], 1201)] {
        if let Some(bytes) = bytes.strip_prefix(bom) {
            return Ok(Coding::new(page)?.decode_lossy(bytes).into_owned());
        }
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.trim_start_matches('\u{feff}').to_owned());
    }
    Ok(Coding::new(code_page)?.decode_lossy(bytes).into_owned())
}

#[cfg(windows)]
async fn output_code_page() -> Result<u16, String> {
    static CODE_PAGE: tokio::sync::OnceCell<u16> = tokio::sync::OnceCell::const_new();
    CODE_PAGE
        .get_or_try_init(|| async {
            // A read-only probe uses the same no-window host as command execution. Reading
            // this property also works in ConstrainedLanguage; setting it would not.
            let (program, arguments) =
                super::native_script_command("[Console]::OutputEncoding.CodePage");
            let output = super::capture_command(
                tokio::process::Command::new(program).args(arguments),
                3_000,
                4_096,
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
            let super::CaptureResult::Completed(output) = output else {
                return Err("Windows shell encoding probe timed out".to_owned());
            };
            if !output.status.success() {
                return Err(format!(
                    "Windows shell encoding probe failed: {}",
                    output.stderr.render()
                ));
            }
            String::from_utf8_lossy(&output.stdout.bytes)
                .trim()
                .parse::<u16>()
                .map_err(|_| "Windows shell returned an invalid output code page".to_owned())
        })
        .await
        .copied()
}

#[cfg(windows)]
pub(super) async fn render_output(output: &super::CapturedOutput) -> (String, String) {
    let legacy = [&output.stdout.bytes, &output.stderr.bytes]
        .iter()
        .any(|bytes| std::str::from_utf8(bytes).is_err());
    let code_page = if legacy {
        output_code_page().await
    } else {
        Ok(65001)
    };
    let render = |stream: &super::CapturedStream| -> Result<String, String> {
        let page = *code_page.as_ref().map_err(Clone::clone)?;
        let text = decode(&stream.bytes, page).map_err(|error| error.to_string())?;
        Ok(if stream.truncated {
            format!("{text}\n… output truncated …")
        } else {
            text
        })
    };
    match (render(&output.stdout), render(&output.stderr)) {
        (Ok(stdout), Ok(stderr)) => (stdout, stderr),
        (stdout, stderr) => {
            let error = stdout
                .as_ref()
                .err()
                .or_else(|| stderr.as_ref().err())
                .cloned()
                .unwrap_or_default();
            (
                stdout.unwrap_or_else(|_| output.stdout.render()),
                format!(
                    "{}\n[Output decoding unavailable: {error}]",
                    stderr.unwrap_or_else(|_| output.stderr.render())
                ),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_chinese_korean_and_oem_output_without_guessing_a_single_locale() {
        for (page, text) in [
            (936, "错误：无法执行命令"),
            (949, "오류: 명령을 실행할 수 없습니다"),
            (437, "échec"),
            (65001, "中文 한국어"),
        ] {
            let bytes = Coding::new(page).unwrap().encode(text).unwrap();
            assert_eq!(decode(&bytes, page).unwrap(), text);
        }
        assert_eq!(
            decode("中文 한국어".as_bytes(), 437).unwrap(),
            "中文 한국어"
        );
        assert_eq!(
            decode(&[0xff, 0xfe, 0x2d, 0x4e, 0x87, 0x65], 437).unwrap(),
            "中文"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn native_powershell_retains_exit_status_and_constrained_language_errors() {
        let page = output_code_page().await.unwrap();
        let (program, args) = super::super::native_script_command(
            "$ExecutionContext.SessionState.LanguageMode = 'ConstrainedLanguage'; [Console]::OutputEncoding.CodePage; 'é中文한국어'; [math]::Round(1.5); exit 7",
        );
        let super::super::CaptureResult::Completed(output) = super::super::capture_command(
            tokio::process::Command::new(program).args(args),
            10_000,
            16_384,
            None,
        )
        .await
        .unwrap() else {
            panic!("PowerShell timed out")
        };
        let (stdout, stderr) = render_output(&output).await;
        assert_eq!(output.status.code(), Some(7));
        assert!(stdout.contains(&page.to_string()), "{stdout}");
        assert!(
            stdout.contains('é'),
            "native output must use the detected code page: {stdout}"
        );
        assert!(
            stderr.contains("MethodInvocationNotSupportedInConstrainedLanguage"),
            "{stderr}"
        );
        assert!(
            !stderr.contains("PropertySetterNotSupportedInConstrainedLanguage"),
            "{stderr}"
        );
        assert!(!stderr.contains("Output decoding unavailable"), "{stderr}");
    }
}
