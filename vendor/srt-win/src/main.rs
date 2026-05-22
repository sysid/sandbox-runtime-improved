//! `srt-win` — CLI for the sandbox-runtime Windows network fence.
//!
//! Subcommands:
//!   group  create | status | delete    — manage the discriminator local group
//!   wfp    install | status | uninstall — manage the persistent WFP filters
//!
//! `status` subcommands write one line of JSON to stdout and exit 0.
//! Mutating subcommands require elevation and write human-readable
//! progress to stderr.

use clap::{Args, Parser, Subcommand};

/// Default group name. Mirrors `srt_win::wfp::DEFAULT_GROUP_NAME`;
/// duplicated here so the CLI struct definitions (which are not
/// `#[cfg(windows)]`-gated) compile on non-Windows hosts where the
/// library crate is empty.
const DEFAULT_GROUP_NAME: &str = "sandbox-runtime-net";

#[derive(Parser)]
#[command(name = "srt-win", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage the local discriminator group.
    Group {
        #[command(subcommand)]
        sub: GroupCmd,
    },
    /// Manage the persistent WFP filters.
    Wfp {
        #[command(subcommand)]
        sub: WfpCmd,
    },
}

/// Group resolution: either by name (looked up via
/// `LookupAccountNameW`) or directly by SID. If both are given the
/// SID wins; `group create`/`delete` always need a name.
#[derive(Args, Clone)]
struct GroupRef {
    /// Group name (local or `DOMAIN\name`). Default
    /// `sandbox-runtime-net`.
    #[arg(long, default_value = DEFAULT_GROUP_NAME)]
    name: String,
    /// Group SID (`S-1-…`). Overrides `--name` for SID resolution.
    /// Use when the group is provisioned by external tooling and name
    /// lookup may be unreliable.
    #[arg(long)]
    group_sid: Option<String>,
}

#[derive(Subcommand)]
enum GroupCmd {
    /// Create the local group and add the current (or `--user-sid`)
    /// user to it. Idempotent. Requires elevation.
    Create {
        #[command(flatten)]
        group: GroupRef,
        /// User SID to add (default: current user).
        #[arg(long)]
        user_sid: Option<String>,
    },
    /// Print group state as JSON: `{state, sid?, warning?}`.
    Status {
        #[command(flatten)]
        group: GroupRef,
    },
    /// Delete the local group. Idempotent. Requires elevation.
    Delete {
        #[command(flatten)]
        group: GroupRef,
    },
}

#[derive(Subcommand)]
enum WfpCmd {
    /// Install (or refresh) the persistent WFP filters for one user.
    /// Idempotent. Requires elevation.
    Install {
        #[command(flatten)]
        group: GroupRef,
        /// User SID to fence (default: current user). Pass explicitly
        /// when running from a SYSTEM-context deployment script.
        #[arg(long)]
        user_sid: Option<String>,
        /// Sublayer GUID. Default is the compile-time constant; pass
        /// when integrating with externally-managed WFP state.
        #[arg(long)]
        sublayer_guid: Option<String>,
    },
    /// Print WFP fence state for one user as JSON:
    /// `{state, filters}`.
    Status {
        #[command(flatten)]
        group: GroupRef,
        #[arg(long)]
        user_sid: Option<String>,
        #[arg(long)]
        sublayer_guid: Option<String>,
    },
    /// Remove the WFP filters for one user (or all srt-win filters
    /// with `--all`). Requires elevation.
    Uninstall {
        #[arg(long)]
        user_sid: Option<String>,
        /// Remove every srt-win filter under the sublayer regardless
        /// of user.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        sublayer_guid: Option<String>,
    },
}

#[cfg(windows)]
fn main() {
    if let Err(e) = run() {
        eprintln!("srt-win: error: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn run() -> anyhow::Result<()> {
    use anyhow::{anyhow, Context};
    use serde_json::json;
    use srt_win::{sid, wfp};

    let cli = Cli::parse();

    let resolve_group_sid = |g: &GroupRef| -> anyhow::Result<String> {
        if let Some(s) = &g.group_sid {
            return Ok(s.clone());
        }
        sid::lookup_account_sid(&g.name)
            .with_context(|| format!("resolve group '{}'", g.name))
    };
    let resolve_user_sid = |u: &Option<String>| -> anyhow::Result<String> {
        match u {
            Some(s) => Ok(s.clone()),
            None => sid::current_user_sid().context("resolve current user"),
        }
    };
    let resolve_sublayer = |s: &Option<String>| -> anyhow::Result<windows::core::GUID> {
        match s {
            Some(g) => wfp::parse_guid(g),
            None => Ok(wfp::DEFAULT_SUBLAYER_GUID),
        }
    };

    match cli.cmd {
        // ─── group ─────────────────────────────────────────────────
        Cmd::Group { sub: GroupCmd::Create { group, user_sid } } => {
            require_elevated()?;
            if group.group_sid.is_some() {
                return Err(anyhow!(
                    "`group create` needs --name; --group-sid is for \
                     referencing an existing group"
                ));
            }
            let user = resolve_user_sid(&user_sid)?;
            wfp::ensure_group(&group.name, &user)?;
            let gsid = sid::lookup_account_sid(&group.name)?;
            eprintln!(
                "srt-win: group '{}' present (sid={gsid}); user {user} added",
                group.name
            );
            eprintln!(
                "srt-win: NOTE — the group SID enters TokenGroups at logon. \
                 Log out and back in before running `wfp install`."
            );
        }
        Cmd::Group { sub: GroupCmd::Status { group } } => {
            // Resolve SID first; if that fails the group is absent.
            let gsid = match (&group.group_sid, sid::lookup_account_sid(&group.name)) {
                (Some(s), _) => s.clone(),
                (None, Ok(s)) => s,
                (None, Err(_)) => {
                    println!("{}", json!({"state": "absent"}));
                    return Ok(());
                }
            };
            let out = match sid::group_state_for_self(&gsid)? {
                sid::GroupState::Enabled => {
                    json!({"state": "ready", "sid": gsid})
                }
                sid::GroupState::Absent => {
                    json!({"state": "created-not-on-token", "sid": gsid})
                }
                sid::GroupState::DenyOnly => json!({
                    "state": "created-not-on-token",
                    "sid": gsid,
                    "warning": "group is deny-only in this token — running \
                                inside a sandbox child?"
                }),
                sid::GroupState::Present => json!({
                    "state": "created-not-on-token",
                    "sid": gsid,
                    "warning": "group present but neither enabled nor \
                                deny-only (unexpected)"
                }),
            };
            println!("{out}");
        }
        Cmd::Group { sub: GroupCmd::Delete { group } } => {
            require_elevated()?;
            if group.group_sid.is_some() {
                return Err(anyhow!(
                    "`group delete` needs --name; cannot delete by SID"
                ));
            }
            wfp::delete_group(&group.name)?;
            eprintln!("srt-win: group '{}' deleted (if it existed)", group.name);
        }

        // ─── wfp ───────────────────────────────────────────────────
        Cmd::Wfp {
            sub: WfpCmd::Install { group, user_sid, sublayer_guid },
        } => {
            require_elevated()?;
            let gsid = resolve_group_sid(&group)?;
            let user = resolve_user_sid(&user_sid)?;
            let sl = resolve_sublayer(&sublayer_guid)?;
            wfp::install_filters(&sl, &gsid, &user)?;
            eprintln!(
                "srt-win: WFP filters installed for user {user} \
                 (group_sid={gsid}, sublayer={sl:?})"
            );
        }
        Cmd::Wfp {
            sub: WfpCmd::Status { group: _, user_sid, sublayer_guid },
        } => {
            let user = resolve_user_sid(&user_sid)?;
            let sl = resolve_sublayer(&sublayer_guid)?;
            let st = wfp::filter_status(&sl, &user)?;
            println!("{}", serde_json::to_string(&st)?);
        }
        Cmd::Wfp {
            sub: WfpCmd::Uninstall { user_sid, all, sublayer_guid },
        } => {
            require_elevated()?;
            let sl = resolve_sublayer(&sublayer_guid)?;
            let target = if all {
                None
            } else {
                Some(resolve_user_sid(&user_sid)?)
            };
            let n = wfp::uninstall_filters(&sl, target.as_deref())?;
            eprintln!("srt-win: removed {n} WFP filter(s)");
        }
    }
    Ok(())
}

#[cfg(windows)]
fn require_elevated() -> anyhow::Result<()> {
    use anyhow::{anyhow, Context};
    use std::ffi::c_void;
    use std::mem::size_of;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcessToken,
    };
    unsafe {
        let mut tok = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok)
            .context("OpenProcessToken")?;
        let mut elev = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        let r = GetTokenInformation(
            tok,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut c_void),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        );
        let _ = CloseHandle(tok);
        r.context("GetTokenInformation(TokenElevation)")?;
        if elev.TokenIsElevated == 0 {
            return Err(anyhow!(
                "this command requires elevation — run from an \
                 administrator prompt"
            ));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    // The clap-derived structs above keep `clap` referenced; just
    // print the platform error.
    let _ = <Cli as clap::CommandFactory>::command();
    eprintln!("srt-win: Windows only");
    std::process::exit(2);
}
