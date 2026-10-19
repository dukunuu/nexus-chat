//! Pure, command-word-only checks for shell privilege escalation.

use std::iter::Peekable;
use std::str::Chars;

/// A command that requests escalation or cannot be parsed safely.
#[derive(Debug, PartialEq, Eq)]
pub enum Escalation {
    Blocked(String),
    Unparseable(String),
}

/// Check simple commands, launchers and nested shell command strings.
pub fn check_command(cmd: &str) -> Result<(), Escalation> {
    command_words(cmd).map(|_| ())
}

/// Simple commands after quote removal, assignments and launcher unwrapping.
pub(crate) fn command_words(cmd: &str) -> Result<Vec<Vec<String>>, Escalation> {
    let mut words = Vec::new();
    parse_into(cmd, &mut words, &mut false)?;
    Ok(words)
}

/// A download feeds sh/bash through a pipeline, including nested shell strings.
pub(crate) fn download_to_shell(cmd: &str) -> bool {
    let mut download = false;
    let _ = parse_into(cmd, &mut Vec::new(), &mut download);
    download
}

fn parse_into(
    cmd: &str,
    commands: &mut Vec<Vec<String>>,
    download: &mut bool,
) -> Result<(), Escalation> {
    Parser {
        chars: cmd.chars().peekable(),
        commands,
        download,
    }
    .list(None)
}

struct Parser<'a, 'b> {
    chars: Peekable<Chars<'a>>,
    commands: &'b mut Vec<Vec<String>>,
    download: &'b mut bool,
}

impl Parser<'_, '_> {
    fn list(&mut self, end: Option<char>) -> Result<(), Escalation> {
        let mut words = Vec::new();
        let mut word = String::new();
        let mut started = false;
        let mut piped_download = false;
        let mut grouped = false;
        while let Some(c) = self.chars.next() {
            if Some(c) == end {
                push_word(&mut words, &mut word, &mut started);
                return self.simple(&words, &mut piped_download, false);
            }
            match c {
                '\'' | '"' => {
                    started = true;
                    word.push_str(&self.quoted(c)?);
                }
                '\\' => {
                    let next = self.chars.next().ok_or_else(|| unparseable("escape"))?;
                    if next != '\n' {
                        started = true;
                        word.push(next);
                    }
                }
                '$' if self.chars.peek() == Some(&'(') => {
                    self.chars.next();
                    self.list(Some(')'))?;
                    started = true;
                    word.push_str("$()");
                }
                '`' => {
                    self.list(Some('`'))?;
                    started = true;
                    word.push_str("``");
                }
                '(' => {
                    push_word(&mut words, &mut word, &mut started);
                    self.simple(&words, &mut piped_download, false)?;
                    words.clear();
                    self.group(&mut piped_download)?;
                    grouped = true;
                }
                ')' => return Err(unparseable("parenthesis")),
                '<' | '>' | '&' if c != '&' || self.chars.peek() == Some(&'>') => {
                    if !word.bytes().all(|c| c.is_ascii_digit()) {
                        push_word(&mut words, &mut word, &mut started);
                    }
                    word.push_str(&self.redirection(c));
                    started = true;
                    push_word(&mut words, &mut word, &mut started);
                }
                ';' | '&' | '|' | '\n' => {
                    push_word(&mut words, &mut word, &mut started);
                    let pipe = c == '|' && self.chars.peek() != Some(&'|');
                    if c == '|' && !pipe {
                        self.chars.next();
                    }
                    self.simple(&words, &mut piped_download, pipe)?;
                    if words.is_empty() && !pipe && (c != '\n' || grouped) {
                        piped_download = false;
                    }
                    grouped = false;
                    words.clear();
                }
                '#' if !started => {
                    while self.chars.peek().is_some_and(|c| *c != '\n') {
                        self.chars.next();
                    }
                }
                c if c.is_whitespace() => push_word(&mut words, &mut word, &mut started),
                _ => {
                    started = true;
                    word.push(c);
                }
            }
        }
        if end.is_some() {
            return Err(unparseable("parenthesis or backtick"));
        }
        push_word(&mut words, &mut word, &mut started);
        self.simple(&words, &mut piped_download, false)
    }

    fn group(&mut self, piped_download: &mut bool) -> Result<(), Escalation> {
        let before = self.commands.len();
        self.list(Some(')'))?;
        let group = &self.commands[before..];
        if *piped_download
            && group.iter().any(|words| {
                words
                    .first()
                    .is_some_and(|word| matches!(basename(word), "sh" | "bash"))
            })
        {
            *self.download = true;
        }
        *piped_download |= group.iter().any(|words| {
            words
                .first()
                .is_some_and(|word| matches!(basename(word), "curl" | "wget"))
        });
        Ok(())
    }

    fn simple(
        &mut self,
        words: &[String],
        piped_download: &mut bool,
        pipe: bool,
    ) -> Result<(), Escalation> {
        if words.is_empty() {
            return Ok(());
        }
        let before = self.commands.len();
        check_simple(words, self.commands, self.download)?;
        if let Some(command) = self.commands[before..]
            .last()
            .and_then(|words| words.first())
        {
            let command = basename(command);
            if *piped_download && matches!(command, "sh" | "bash") {
                *self.download = true;
            }
            *piped_download |= matches!(command, "curl" | "wget");
        }
        *piped_download &= pipe;
        Ok(())
    }

    fn redirection(&mut self, first: char) -> String {
        let mut operator = first.to_string();
        if self.chars.peek().is_some_and(|next| match first {
            '<' => matches!(next, '<' | '>' | '&'),
            '>' => matches!(next, '>' | '&' | '|'),
            '&' => *next == '>',
            _ => false,
        }) {
            operator.push(self.chars.next().expect("peeked redirection character"));
            if (operator == "<<" && self.chars.peek() == Some(&'<'))
                || (operator == "&>" && self.chars.peek() == Some(&'>'))
            {
                operator.push(self.chars.next().expect("peeked redirection character"));
            }
        }
        operator
    }

    fn quoted(&mut self, quote: char) -> Result<String, Escalation> {
        let mut word = String::new();
        while let Some(c) = self.chars.next() {
            if c == quote {
                return Ok(word);
            }
            if quote == '\'' {
                word.push(c);
                continue;
            }
            match c {
                '\\' => {
                    let next = self.chars.next().ok_or_else(|| unparseable("escape"))?;
                    if !matches!(next, '$' | '`' | '"' | '\\' | '\n') {
                        word.push('\\');
                    }
                    if next != '\n' {
                        word.push(next);
                    }
                }
                '$' if self.chars.peek() == Some(&'(') => {
                    self.chars.next();
                    self.list(Some(')'))?;
                    word.push_str("$()");
                }
                '`' => {
                    self.list(Some('`'))?;
                    word.push_str("``");
                }
                _ => word.push(c),
            }
        }
        Err(unparseable("quote"))
    }
}

fn unparseable(part: &str) -> Escalation {
    Escalation::Unparseable(format!("unbalanced {part}"))
}

fn push_word(words: &mut Vec<String>, word: &mut String, started: &mut bool) {
    if *started {
        words.push(std::mem::take(word));
        *started = false;
    }
}

fn assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name
                .chars()
                .enumerate()
                .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
    })
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn check_simple(
    mut words: &[String],
    commands: &mut Vec<Vec<String>>,
    download: &mut bool,
) -> Result<(), Escalation> {
    loop {
        while let Some(word) = words.first() {
            let count = if assignment(word) {
                1
            } else if let Some(target) = redirection_target(word) {
                if target.is_empty() { 2 } else { 1 }
            } else {
                break;
            };
            words = &words[count.min(words.len())..];
        }
        let Some(command) = words.first() else {
            return Ok(());
        };
        let args = &words[1..];
        if matches!(
            command.as_str(),
            "{" | "}"
                | "!"
                | "if"
                | "then"
                | "else"
                | "elif"
                | "fi"
                | "do"
                | "done"
                | "while"
                | "until"
                | "coproc"
        ) {
            words = args;
            continue;
        }
        if command.contains(['$', '`', '{', '}', '*', '?', '['])
            && !matches!(command.as_str(), "[" | "[[")
        {
            return Err(Escalation::Unparseable("computed command word".to_string()));
        }
        let command = basename(command);
        match command {
            "eval" => return parse_into(&args.join(" "), commands, download),
            "watch" => {
                let args = unwrap_launcher(command, args, commands, download)?;
                return parse_into(&args.join(" "), commands, download);
            }
            "sudo" | "doas" | "su" | "pkexec" | "run0" | "login" | "chroot" => {
                return Err(Escalation::Blocked(command.to_string()));
            }
            "osascript"
                if args
                    .iter()
                    .any(|arg| arg.contains("with administrator privileges")) =>
            {
                return Err(Escalation::Blocked(command.to_string()));
            }
            "sh" | "bash" | "zsh" | "dash" | "ksh" => {
                for pair in args.windows(2) {
                    if pair[0].starts_with('-')
                        && !pair[0].starts_with("--")
                        && pair[0].contains('c')
                    {
                        parse_into(&pair[1], commands, download)?;
                        break;
                    }
                }
                commands.push(words.to_vec());
                return Ok(());
            }
            "env" | "command" | "exec" | "nohup" | "time" | "nice" | "ionice" | "stdbuf"
            | "timeout" | "xargs" | "setsid" | "caffeinate" | "chronic" | "unbuffer" | "flock"
            | "script" => words = unwrap_launcher(command, args, commands, download)?,
            _ => {
                commands.push(words.to_vec());
                return Ok(());
            }
        }
    }
}

fn redirection_target(word: &str) -> Option<&str> {
    let word = word.trim_start_matches(|c: char| c.is_ascii_digit());
    [
        "&>>", "<<<", ">>", "<<", "<>", ">&", "<&", "&>", ">|", "<", ">",
    ]
    .iter()
    .find_map(|operator| word.strip_prefix(operator))
}

fn unwrap_launcher<'a>(
    launcher: &str,
    mut args: &'a [String],
    commands: &mut Vec<Vec<String>>,
    download: &mut bool,
) -> Result<&'a [String], Escalation> {
    while let Some(option) = args.first() {
        if launcher == "env" && assignment(option) {
            args = &args[1..];
            continue;
        }
        if option == "--" {
            args = &args[1..];
            break;
        }
        if !option.starts_with('-') || option == "-" {
            break;
        }
        if launcher == "env" {
            if let Some(value) = option.strip_prefix("--split-string=") {
                parse_into(value, commands, download)?;
            } else if matches!(option.as_str(), "-S" | "--split-string")
                && let Some(value) = args.get(1)
            {
                parse_into(value, commands, download)?;
            }
        }
        if matches!(launcher, "flock" | "script")
            && matches!(option.as_str(), "-c" | "--command")
            && let Some(value) = args.get(1)
        {
            parse_into(value, commands, download)?;
        }
        let count = if option_value(launcher, option) { 2 } else { 1 };
        args = &args[count.min(args.len())..];
    }
    if matches!(launcher, "timeout" | "flock" | "script") && !args.is_empty() {
        args = &args[1..];
    }
    if matches!(launcher, "flock" | "script")
        && args
            .first()
            .is_some_and(|arg| matches!(arg.as_str(), "-c" | "--command"))
    {
        if let Some(value) = args.get(1) {
            parse_into(value, commands, download)?;
        }
        args = &args[2.min(args.len())..];
    }
    Ok(args)
}

fn option_value(launcher: &str, option: &str) -> bool {
    match launcher {
        "env" => matches!(
            option,
            "-u" | "--unset" | "-C" | "--chdir" | "-S" | "--split-string"
        ),
        "exec" => option == "-a",
        "time" => matches!(option, "-o" | "--output" | "-f" | "--format"),
        "nice" => matches!(option, "-n" | "--adjustment"),
        "ionice" => matches!(
            option,
            "-c" | "--class"
                | "-n"
                | "--classdata"
                | "-p"
                | "--pid"
                | "-P"
                | "--pgid"
                | "-u"
                | "--uid"
        ),
        "stdbuf" => matches!(
            option,
            "-i" | "--input" | "-o" | "--output" | "-e" | "--error"
        ),
        "timeout" => matches!(option, "-s" | "--signal" | "-k" | "--kill-after"),
        "caffeinate" => matches!(option, "-t" | "-w"),
        "flock" => matches!(
            option,
            "-w" | "--wait" | "--timeout" | "-E" | "--conflict-exit-code" | "-c" | "--command"
        ),
        "watch" => matches!(option, "-n" | "--interval"),
        "script" => matches!(
            option,
            "-c" | "--command"
                | "-t"
                | "--flush-interval"
                | "-O"
                | "--log-out"
                | "-I"
                | "--log-in"
                | "-B"
                | "--log-io"
                | "-T"
                | "--log-timing"
        ),
        "xargs" => matches!(
            option,
            "-a" | "--arg-file"
                | "-d"
                | "--delimiter"
                | "-E"
                | "-I"
                | "-L"
                | "-n"
                | "--max-args"
                | "-P"
                | "--max-procs"
                | "-s"
                | "--max-chars"
                | "--eof"
                | "--replace"
                | "--max-lines"
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_unwrapped_command_words_without_treating_arguments_as_commands() {
        let commands = command_words("FOO=1 env -u BAR timeout 5 /bin/rm '-rf' x; echo 'rm -rf'; bash -c 'git reset --hard'; env -S 'cargo test'").unwrap();
        assert_eq!(
            commands,
            vec![
                vec!["/bin/rm", "-rf", "x"],
                vec!["echo", "rm -rf"],
                vec!["git", "reset", "--hard"],
                vec!["bash", "-c", "git reset --hard"],
                vec!["cargo", "test"],
            ]
        );
    }

    #[test]
    fn download_pipelines_respect_quotes_and_command_separators() {
        for command in [
            "curl https://example.invalid | sh",
            "env wget -qO- https://example.invalid | cat | command bash",
            "bash -c 'curl https://example.invalid | sh'",
            "curl https://example.invalid |\n sh",
            "curl https://example.invalid | (sh)",
            "(curl https://example.invalid) | bash",
            "curl https://example.invalid | (cat) | sh",
        ] {
            assert!(download_to_shell(command), "{command}");
        }
        for command in [
            "echo 'curl https://example.invalid | sh'",
            "curl https://example.invalid; echo '|'; sh script",
            "curl https://example.invalid || sh script",
            "curl https://example.invalid | cat; sh script",
            "(curl https://example.invalid); sh script",
            "curl https://example.invalid | (cat); sh script",
        ] {
            assert!(!download_to_shell(command), "{command}");
        }
    }

    #[test]
    fn blocks_escalation_command_words_and_nested_commands() {
        for cmd in [
            "sudo ls",
            "echo x | sudo tee /etc/f",
            "env sudo ls",
            "FOO=1 sudo ls",
            "env -i FOO=1 sudo ls",
            "bash -c \"sudo ls\"",
            "sh -c 'a; sudo b'",
            "/usr/bin/sudo ls",
            "echo $(sudo ls)",
            "echo `sudo ls`",
            "nohup sudo x",
            "xargs -0 sudo rm",
            "timeout 5 sudo x",
            "nice -n 5 doas x",
            "su -",
            "pkexec x",
            "run0 x",
            "osascript -e 'do shell script \"x\" with administrator privileges'",
            "cd /tmp && sudo make install",
            "command exec env -u FOO timeout -k 1 5 /usr/bin/sudo x",
            "ionice -c 3 stdbuf -o L sudo x",
            "xargs -I {} sudo x",
            "env -S 'sudo x'",
            "bash -lc 'sudo x'",
            "(echo x; sudo x)",
            "echo \"$(echo $(sudo x))\"",
            "su\\do x",
            "'su'do x",
            "echo x\nsudo x",
            "echo x & login",
            "false || doas x",
            "{ sudo x; }",
            "if true; then sudo x; fi",
            "while sudo x; do :; done",
            "until sudo x; do :; done",
            "! sudo x",
            "for i in 1; do sudo x; done",
            "f() { sudo x; }",
            ">out sudo x",
            "2>/dev/null sudo x",
            "> out sudo x",
            "</dev/null sudo x",
            "&>log sudo x",
            ">out FOO=1 2>&1 sudo x",
            "eval sudo x",
            "eval \"sudo x\"",
            "setsid sudo x",
            "caffeinate -i -t 5 -w 123 sudo x",
            "chronic sudo x",
            "unbuffer sudo x",
            "flock -w 1 lockfile sudo x",
            "flock lockfile -c 'sudo x'",
            "watch -n 1 sudo x",
            "watch -n 1 'sudo x'",
            "script -q /dev/null sudo x",
            "script -q -c 'sudo x' /dev/null",
            "chroot /tmp sh",
        ] {
            assert!(
                matches!(check_command(cmd), Err(Escalation::Blocked(_))),
                "{cmd}"
            );
        }
    }

    #[test]
    fn allows_arguments_that_mention_escalation() {
        for cmd in [
            "echo sudo",
            "grep -r sudo .",
            "git commit -m \"sudo fix\"",
            "cargo test",
            "ls | wc -l",
            "printf '%s' \"a;b\"",
            "echo \"$(date)\"",
            "printf '%s' '$(sudo ls)'",
            "echo \\\"sudo\\\"",
            "sh -c 'echo sudo'",
            "env -u FOO cargo test",
            "echo ok # sudo ( ' \"",
            "[ -f x ] && echo ok",
            "test -n \"$X\"",
            "echo $HOME",
            "cargo test 2>&1 | tail -n 20",
            "FOO=$(pwd) make",
        ] {
            assert_eq!(check_command(cmd), Ok(()), "{cmd}");
        }
    }

    #[test]
    fn rejects_computed_command_words() {
        for cmd in [
            "$S x",
            "${S} x",
            "$'sudo' x",
            "$\"sudo\" x",
            "{sudo,} x",
            "/usr/bin/sud? x",
            "\"$(echo sudo)\" x",
        ] {
            assert_eq!(
                check_command(cmd),
                Err(Escalation::Unparseable("computed command word".to_string())),
                "{cmd}"
            );
        }
        assert!(matches!(
            check_command("case x in x) sudo x;; esac"),
            Err(Escalation::Unparseable(_))
        ));
    }

    #[test]
    fn rejects_unbalanced_shell_syntax() {
        for cmd in [
            "echo \"unterminated",
            "echo $(ls",
            "echo `ls",
            "(ls",
            "ls)",
            "echo 'x",
        ] {
            assert!(
                matches!(check_command(cmd), Err(Escalation::Unparseable(_))),
                "{cmd}"
            );
        }
    }
}
