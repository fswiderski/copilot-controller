pub(super) fn parse(args: &[String]) -> Result<(), String> {
    if args == ["--device", "mx-creative"] {
        Ok(())
    } else {
        Err("review-input requires exactly --device mx-creative".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_exact_private_device_form() {
        assert!(parse(&["--device".into(), "mx-creative".into()]).is_ok());
        for args in [
            vec![],
            vec!["--device".into(), "ct".into()],
            vec!["--device".into(), "micro".into()],
            vec!["mx-creative".into()],
            vec!["--device=mx-creative".into()],
            vec!["--device".into(), "mx-creative".into(), "--extra".into()],
        ] {
            assert!(parse(&args).is_err());
        }
    }
}
