//! Asking the user a yes/no question.

use std::io::{self, BufRead, Write};

/// Asks `question` and reads one line from `input`.
///
/// Only `y` or `yes` (any case) means yes. Anything else, including an empty
/// line or end of input, means no, so a script with no terminal can never
/// confirm by accident.
///
/// Errors if the question cannot be written or the answer cannot be read.
pub fn confirm<I: BufRead + ?Sized, O: Write + ?Sized>(
    question: &str,
    input: &mut I,
    output: &mut O,
) -> io::Result<bool> {
    write!(output, "{question} [y/N] ")?;
    output.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(answer: &str) -> (bool, String) {
        let mut output = Vec::new();
        let result = confirm("Proceed?", &mut answer.as_bytes(), &mut output).unwrap();
        (result, String::from_utf8(output).unwrap())
    }

    #[test]
    fn y_and_yes_in_any_case_mean_yes() {
        for answer in ["y\n", "Y\n", "yes\n", "YES\n", "Yes\n", "  y  \n", "y"] {
            assert!(ask(answer).0, "{answer:?} should mean yes");
        }
    }

    #[test]
    fn everything_else_means_no() {
        for answer in [
            "n\n",
            "no\n",
            "\n",
            "",
            "yep\n",
            "ye\n",
            "sure\n",
            "1\n",
            "yes please\n",
        ] {
            assert!(!ask(answer).0, "{answer:?} should mean no");
        }
    }

    #[test]
    fn end_of_input_means_no() {
        assert!(!ask("").0);
    }

    #[test]
    fn the_question_is_shown_with_the_default() {
        assert_eq!(ask("n\n").1, "Proceed? [y/N] ");
    }

    #[test]
    fn only_the_first_line_is_read() {
        let mut input = "n\ny\n".as_bytes();
        let mut output = Vec::new();

        let first = confirm("a?", &mut input, &mut output).unwrap();
        let second = confirm("b?", &mut input, &mut output).unwrap();

        assert!(!first);
        assert!(second);
    }

    #[test]
    fn a_read_error_is_reported() {
        struct Broken;
        impl io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("closed"))
            }
        }
        let mut input = io::BufReader::new(Broken);

        let error = confirm("?", &mut input, &mut Vec::new()).unwrap_err();

        assert_eq!(error.to_string(), "closed");
    }

    #[test]
    fn a_write_error_is_reported() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let error = confirm("?", &mut "y\n".as_bytes(), &mut Broken).unwrap_err();

        assert_eq!(error.to_string(), "closed");
    }
}
