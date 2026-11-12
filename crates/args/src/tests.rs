use super::*;

static INSPECT: Spec = Spec {
    verb: "inspect",
    summary: "Show what a package is",
    positionals: &[Positional {
        name: "INPUT",
        help: "package",
    }],
    options: &[
        JSON,
        Opt {
            long: "out",
            short: Some('o'),
            value: Some("FILE"),
            help: "target",
        },
    ],
    one_of: &[],
};
static ART: Spec = Spec {
    verb: "artwork",
    summary: "Save artwork",
    positionals: &[],
    options: &[Opt::flag("icon0", "icon"), Opt::flag("pic1", "pic")],
    one_of: &["icon0", "pic1"],
};
static CMD: Command = Command {
    name: "athena",
    summary: "inspect",
    verbs: &[&INSPECT, &ART],
};

fn argv(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn run(args: &[&str]) -> Args {
    match parse(&CMD, &argv(args)) {
        Ok(Invocation::Run(_, args)) => args,
        other => panic!("{other:?}"),
    }
}

#[test]
fn flags_values_and_positionals() {
    let a = run(&["inspect", "--json", "x.pkg", "-o", "a"]);
    assert_eq!(a.positional(0), Some(OsStr::new("x.pkg")));
    assert!(a.flag("json"));
    assert_eq!(a.value("out"), Some(OsStr::new("a")));
    let a = run(&["inspect", "--out=b", "--out", "c", "x"]);
    assert_eq!(a.value("out"), Some(OsStr::new("c")));
    // After `--`, dashes are positionals.
    let a = run(&["inspect", "--", "--json"]);
    assert_eq!(a.positional(0), Some(OsStr::new("--json")));
    assert!(!a.flag("json"));
    let a = run(&["inspect", "-"]);
    assert_eq!(a.positional(0), Some(OsStr::new("-")));
}

#[cfg(unix)]
#[test]
fn non_utf8_positionals_survive() {
    use std::os::unix::ffi::OsStringExt;
    let raw = OsString::from_vec(vec![b'f', 0xff]);
    let Ok(Invocation::Run(_, a)) = parse(&CMD, &[OsString::from("inspect"), raw.clone()]) else {
        panic!("parse");
    };
    assert_eq!(a.positional(0), Some(raw.as_os_str()));
}

#[test]
fn usage_errors() {
    for bad in [
        &[][..],
        &["nope"],
        &["inspect"],
        &["inspect", "a", "b"],
        &["inspect", "--bogus", "a"],
        &["inspect", "-x", "a"],
        &["inspect", "-oj", "a"],
        &["inspect", "a", "--out"],
        &["inspect", "a", "--json=1"],
        &["artwork"],
        &["artwork", "--icon0", "--pic1"],
    ] {
        assert!(parse(&CMD, &argv(bad)).is_err(), "{bad:?}");
    }
    assert!(run(&["artwork", "--pic1"]).flag("pic1"));
}

#[test]
fn help_is_generated_from_the_spec() {
    let Ok(Invocation::Help(text)) = parse(&CMD, &argv(&["--help"])) else {
        panic!()
    };
    assert!(text.contains("athena inspect INPUT [OPTIONS]"));
    assert!(text.contains("athena artwork (--icon0|--pic1)"));
    for args in [
        &["help", "inspect"][..],
        &["inspect", "-h"],
        &["inspect", "x", "--help"],
    ] {
        let Ok(Invocation::Help(text)) = parse(&CMD, &argv(args)) else {
            panic!("{args:?}")
        };
        assert!(text.contains("-o, --out FILE"), "{text}");
        assert!(text.contains("INPUT"));
    }
    assert!(parse(&CMD, &argv(&["help", "nope"])).is_err());
}

#[test]
fn fail_reports_and_maps_exit_code() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut io = Io {
        out: &mut out,
        err: &mut err,
    };
    assert_eq!(fail(&mut io, "athena", &UsageError("bad".into())), 2);
    assert_eq!(err, b"athena: bad\n");
}
