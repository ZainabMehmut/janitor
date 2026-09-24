use super::*;

#[test]
fn test_drop_env() {
    let mut args = vec![
        "FOO=bar".to_string(),
        "BAZ=qux".to_string(),
        "actual".to_string(),
        "command".to_string(),
    ];
    drop_env(&mut args);
    assert_eq!(args, vec!["actual", "command"]);
}

#[test]
fn test_drop_env_no_env_vars() {
    let mut args = vec!["actual".to_string(), "command".to_string()];
    let original = args.clone();
    drop_env(&mut args);
    assert_eq!(args, original);
}

#[test]
fn test_drop_env_empty() {
    let mut args = vec![];
    drop_env(&mut args);
    assert!(args.is_empty());
}

#[test]
fn test_drop_env_only_env_vars() {
    let mut args = vec!["FOO=bar".to_string(), "BAZ=qux".to_string()];
    drop_env(&mut args);
    assert!(args.is_empty());
}

#[test]
fn test_drop_env_with_equals_in_arg() {
    let mut args = vec![
        "FOO=bar".to_string(),
        "command".to_string(),
        "--option=value".to_string(),
    ];
    drop_env(&mut args);
    assert_eq!(args, vec!["command", "--option=value"]);
}

fn reset_error() -> BranchOpenError {
    BranchOpenError::Unavailable {
        url: url::Url::parse("http://localhost:9923/codebase").unwrap(),
        description: "Transport error: Connection closed early".to_string(),
    }
}

fn other_error() -> BranchOpenError {
    BranchOpenError::Missing {
        url: url::Url::parse("http://localhost:9923/codebase").unwrap(),
        description: "Local branch missing".to_string(),
    }
}

#[test]
fn test_open_branch_retrying_retries_once_after_a_reset() {
    let mut calls = 0;
    let result = open_branch_retrying(|| {
        calls += 1;
        if calls == 1 {
            Err(reset_error())
        } else {
            Ok("branch")
        }
    });
    assert_eq!(result.unwrap(), "branch");
    assert_eq!(calls, 2);
}

#[test]
fn test_open_branch_retrying_does_not_retry_other_errors() {
    let mut calls = 0;
    let result = open_branch_retrying(|| {
        calls += 1;
        Err::<&str, _>(other_error())
    });
    assert!(result.is_err());
    assert_eq!(calls, 1, "a non-reset failure must not be retried");
}

#[test]
fn test_open_branch_retrying_gives_up_after_one_retry() {
    let mut calls = 0;
    let result = open_branch_retrying(|| {
        calls += 1;
        Err::<&str, _>(reset_error())
    });
    assert!(result.is_err());
    assert_eq!(calls, 2, "retry happens once, not in a loop");
}

#[test]
fn test_open_branch_retrying_does_not_call_twice_on_success() {
    let mut calls = 0;
    let result = open_branch_retrying(|| {
        calls += 1;
        Ok::<_, BranchOpenError>("branch")
    });
    assert_eq!(result.unwrap(), "branch");
    assert_eq!(calls, 1);
}
