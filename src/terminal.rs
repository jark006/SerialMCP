use std::sync::atomic::{AtomicU64, Ordering};

pub fn key_bytes(key: &str) -> Option<&'static [u8]> {
    Some(match key {
        "ctrl_c" => b"\x03",
        "ctrl_d" => b"\x04",
        "ctrl_z" => b"\x1a",
        "ctrl_l" => b"\x0c",
        "escape" => b"\x1b",
        "tab" => b"\t",
        "enter" => b"\r",
        "backspace" => b"\x7f",
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        _ => return None,
    })
}

static NEXT_COMMAND: AtomicU64 = AtomicU64::new(1);

pub struct ShellCommand {
    pub payload: Vec<u8>,
    pub begin: Vec<u8>,
    pub rc: Vec<u8>,
    pub end: Vec<u8>,
}
impl ShellCommand {
    pub fn new(command: &str) -> Self {
        Self::build(command, false)
    }

    pub fn for_exec(command: &str) -> Self {
        Self::build(command, true)
    }

    fn build(command: &str, noninteractive: bool) -> Self {
        let token = format!(
            "SMCP_{}_{}",
            std::process::id(),
            NEXT_COMMAND.fetch_add(1, Ordering::Relaxed)
        );
        // Octal escapes keep literal markers out of the terminal's echoed command.
        let (setup, quoted) = if command.len() > 128 {
            let variable = format!("_cmd_{token}");
            let mut setup = format!("{variable}=''\n");
            for fragment in command_fragments(command) {
                setup.push_str(&format!("{variable}=\"${variable}\"'{fragment}'\n"));
            }
            (setup, format!("\"${variable}\""))
        } else {
            (
                String::new(),
                format!("'{}'", command.replace('\'', "'\\''")),
            )
        };
        let input = if noninteractive {
            " < /dev/null"
        } else if command.len() > 128 {
            " <&3 3<&-"
        } else {
            ""
        };
        // An eval alias must not replace the requested command with its own exit code.
        let body = format!(
            "set +e\n{setup}command printf '\\036{token}_BEGIN\\037'; ( command eval {quoted} ){input}; command printf '\\036{token}_RC:%s\\037\\036{token}_END\\037' \"$?\"\n"
        );
        let payload = if command.len() > 128 {
            // A here-document feeds a noninteractive child incrementally. One
            // giant compound command can exceed BusyBox's interactive parser.
            let delimiter = format!("{token}_SCRIPT");
            let saved_input = if noninteractive { "" } else { " 3<&0" };
            format!("command -p sh{saved_input} <<'{delimiter}'\n{body}{delimiter}\n").into_bytes()
        } else {
            format!("( {body})\n").into_bytes()
        };
        Self {
            payload,
            begin: format!("\x1e{token}_BEGIN\x1f").into_bytes(),
            rc: format!("\x1e{token}_RC:").into_bytes(),
            end: format!("\x1e{token}_END\x1f").into_bytes(),
        }
    }

    pub fn parse(&self, raw: &[u8]) -> (Vec<u8>, Option<i32>) {
        let start = raw
            .windows(self.begin.len())
            .position(|w| w == self.begin)
            .map(|i| i + self.begin.len());
        let Some(start) = start else {
            return (raw.to_vec(), None);
        };
        let body = &raw[start..];
        if let Some(index) = body.windows(self.rc.len()).rposition(|w| w == self.rc) {
            let tail = &body[index + self.rc.len()..];
            if let Some(end) = tail.iter().position(|b| *b == 0x1f)
                && tail[end + 1..] == self.end
            {
                let rc = std::str::from_utf8(&tail[..end])
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .filter(|code| (0..=255).contains(code));
                return (body[..index].to_vec(), rc);
            }
        }
        (body.to_vec(), None)
    }
}

// Embedded line editors often cap each physical input line at 256/1024 bytes.
// Append quoted pieces to a private subshell variable so neither a physical
// input line nor one lexer token has to contain the entire command.
fn command_fragments(command: &str) -> Vec<String> {
    let mut fragments = Vec::new();
    let mut quoted = String::new();
    for character in command.chars() {
        let fragment = if character == '\'' {
            "'\\''".to_string()
        } else {
            character.to_string()
        };
        if quoted.len() + fragment.len() > 128 {
            fragments.push(std::mem::take(&mut quoted));
        }
        quoted.push_str(&fragment);
    }
    if !quoted.is_empty() {
        fragments.push(quoted);
    }
    fragments
}

pub fn strip_ansi_bytes(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(data.len());
    let mut index = 0;
    while index < data.len() {
        if data[index] != 0x1B {
            if data[index] == 0x08 {
                if output
                    .last()
                    .is_some_and(|byte| !matches!(byte, b'\r' | b'\n'))
                {
                    while let Some(byte) = output.pop() {
                        if byte & 0xC0 != 0x80 {
                            break;
                        }
                    }
                }
            } else if data[index] >= 0x20 || matches!(data[index], b'\r' | b'\n' | b'\t') {
                output.push(data[index]);
            }
            index += 1;
            continue;
        }

        index += 1;
        if index >= data.len() {
            break;
        }
        match data[index] {
            b'[' => {
                index += 1;
                while index < data.len() {
                    let byte = data[index];
                    index += 1;
                    if (0x40..=0x7E).contains(&byte) {
                        break;
                    }
                }
            }
            b']' | b'P' | b'_' | b'^' => {
                index += 1;
                while index < data.len() {
                    if data[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if data[index] == 0x1B && data.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            b'(' | b')' | b'*' | b'+' => index = (index + 2).min(data.len()),
            _ => index += 1,
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_commands_keep_physical_lines_within_embedded_terminal_limit() {
        let command = format!("printf '%s' '{}'", "单引号'\\".repeat(2000));
        let shell = ShellCommand::new(&command);
        assert!(
            shell
                .payload
                .split(|byte| *byte == b'\n')
                .all(|line| line.len() <= 256)
        );
        assert!(
            !shell
                .payload
                .windows(shell.end.len())
                .any(|window| window == shell.end)
        );
    }
    #[test]
    fn shell_completion_requires_actual_markers_and_numeric_exit_code() {
        let shell = ShellCommand::new("echo 'hello'\nexit 7");
        assert!(
            !shell
                .payload
                .windows(shell.end.len())
                .any(|w| w == shell.end)
        );
        assert_eq!(shell.parse(&shell.payload).1, None);
        let mut response = shell.payload.clone(); // Terminal echo and a stale prompt.
        response.extend_from_slice(b"\r\n# ");
        response.extend_from_slice(&shell.begin);
        response.extend_from_slice(b"hello\r\n");
        response.extend_from_slice(&shell.rc);
        response.extend_from_slice(b"7\x1f");
        response.extend_from_slice(&shell.end);
        assert_eq!(shell.parse(&response), (b"hello\r\n".to_vec(), Some(7)));
        response.pop();
        assert_eq!(shell.parse(&response).1, None);
        assert_eq!(strip_ansi_bytes("中\u{8}文".as_bytes()), "文".as_bytes());
    }

    #[test]
    #[ignore = "requires a POSIX shell; set SERIAL_MCP_TEST_SHELL on Windows"]
    fn posix_shell_executes_quoted_multiline_command() {
        use std::{
            io::Write,
            process::{Command, Stdio},
        };
        let shell = ShellCommand::new(
            "printf '%s\\n' \"it's 中文\"\nprintf '%s' 'no-final-newline'\nexit 7",
        );
        let executable =
            std::env::var("SERIAL_MCP_TEST_SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut child = Command::new(&executable)
            .arg("-s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&shell.payload)
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            shell.parse(&output.stdout),
            ("it's 中文\nno-final-newline".as_bytes().to_vec(), Some(7))
        );
        for value in ["A".repeat(4096), "quote ' 中文 \\".repeat(300)] {
            let quoted_value = format!("'{}'", value.replace('\'', "'\\''"));
            let shell = ShellCommand::new(&format!("printf '%s' {quoted_value}"));
            let mut child = Command::new(&executable)
                .arg("-s")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(&shell.payload)
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success());
            assert_eq!(shell.parse(&output.stdout), (value.into_bytes(), Some(0)));
        }
    }

    #[test]
    #[ignore = "requires a POSIX shell; set SERIAL_MCP_TEST_SHELL on Windows"]
    fn exec_stdin_is_eof_and_terminal_stdin_survives_long_script() {
        use std::{
            fs,
            io::Write,
            process::{Command, Stdio},
        };
        let executable =
            std::env::var("SERIAL_MCP_TEST_SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let path =
            std::env::temp_dir().join(format!("serial-mcp-stdin-test-{}.txt", std::process::id()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(b"provided\n").unwrap();
        drop(file);
        for padding in [0, 300] {
            let command = format!(
                "read value; code=$?; printf '%s:%s' \"$code\" \"$value\"\n: # {}",
                "A".repeat(padding)
            );
            for terminal in [false, true] {
                let shell = if terminal {
                    ShellCommand::new(&command)
                } else {
                    ShellCommand::for_exec(&command)
                };
                let output = Command::new(&executable)
                    .arg("-c")
                    .arg(String::from_utf8(shell.payload.clone()).unwrap())
                    .stdin(Stdio::from(fs::File::open(&path).unwrap()))
                    .output()
                    .unwrap();
                assert!(output.status.success());
                assert_eq!(
                    shell.parse(&output.stdout),
                    (
                        if terminal {
                            b"0:provided".to_vec()
                        } else {
                            b"1:".to_vec()
                        },
                        Some(0)
                    )
                );
            }
        }
        fs::remove_file(path).unwrap();
        for command in [
            "printf 'piped\\n' | { read value; printf '%s' \"$value\"; }",
            "read value <<'DATA'\nredirected\nDATA\nprintf '%s' \"$value\"",
        ] {
            for padding in [0, 300] {
                let shell =
                    ShellCommand::for_exec(&format!("{command}\n: # {}", "A".repeat(padding)));
                let output = Command::new(&executable)
                    .arg("-c")
                    .arg(String::from_utf8(shell.payload.clone()).unwrap())
                    .output()
                    .unwrap();
                assert!(output.status.success());
                assert_eq!(
                    shell.parse(&output.stdout),
                    (
                        if command.starts_with("printf") {
                            b"piped".to_vec()
                        } else {
                            b"redirected".to_vec()
                        },
                        Some(0)
                    )
                );
            }
        }
    }

    #[test]
    #[ignore = "requires a POSIX shell; set SERIAL_MCP_TEST_SHELL on Windows"]
    fn markers_ignore_printf_function_and_long_launch_preserves_path() {
        use std::process::Command;
        let executable =
            std::env::var("SERIAL_MCP_TEST_SHELL").unwrap_or_else(|_| "/bin/sh".into());
        for padding in [0, 300] {
            let shell =
                ShellCommand::for_exec(&format!("echo stable-output\n: # {}", "A".repeat(padding)));
            let script = format!(
                "printf() {{ command printf 'shadowed\\n'; }}\n{}",
                String::from_utf8(shell.payload.clone()).unwrap()
            );
            let output = Command::new(&executable)
                .arg("-c")
                .arg(script)
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(
                shell.parse(&output.stdout),
                (b"stable-output\n".to_vec(), Some(0))
            );
            let shell = ShellCommand::for_exec(&format!(
                "command printf '%s' \"$PATH\"\n: # {}",
                "A".repeat(padding)
            ));
            let output = Command::new(&executable)
                .arg("-c")
                .arg(format!(
                    "PATH=/nonexistent\n{}",
                    String::from_utf8(shell.payload.clone()).unwrap()
                ))
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(
                shell.parse(&output.stdout),
                (b"/nonexistent".to_vec(), Some(0))
            );
        }
    }

    #[test]
    #[ignore = "requires a POSIX shell; set SERIAL_MCP_TEST_SHELL on Windows"]
    fn eval_alias_does_not_replace_user_command() {
        use std::process::Command;
        let executable =
            std::env::var("SERIAL_MCP_TEST_SHELL").unwrap_or_else(|_| "/bin/sh".into());
        for padding in [0, 300] {
            let command = format!(
                "command printf actual-output; exit 7\n: # {}",
                "A".repeat(padding)
            );
            for terminal in [false, true] {
                let shell = if terminal {
                    ShellCommand::new(&command)
                } else {
                    ShellCommand::for_exec(&command)
                };
                let script = format!(
                    "if command -v shopt >/dev/null 2>&1; then shopt -s expand_aliases; fi\nalias eval='echo shadowed-eval'\neval alias-probe\n{}",
                    String::from_utf8(shell.payload.clone()).unwrap()
                );
                let output = Command::new(&executable)
                    .arg("-c")
                    .arg(script)
                    .output()
                    .unwrap();
                assert!(output.status.success());
                assert!(output.stdout.starts_with(b"shadowed-eval alias-probe\n"));
                assert_eq!(
                    shell.parse(&output.stdout),
                    (b"actual-output".to_vec(), Some(7)),
                    "padding={padding}, terminal={terminal}"
                );
            }
        }
    }
}
