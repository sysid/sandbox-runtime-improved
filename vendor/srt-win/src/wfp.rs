//! Windows Filtering Platform (WFP) filter management and local-group
//! provisioning for the sandbox-runtime Windows network fence.
//!
//! ## Design
//!
//! At install time we create a local group (default
//! `sandbox-runtime-net`), add the target user, and persist three WFP
//! filters per user at each of `FWPM_LAYER_ALE_AUTH_CONNECT_V4` and
//! `_V6`, all under one persistent sublayer:
//!
//!   1. **PERMIT** (high weight) — `ALE_USER_ID` matches an SD granting
//!      `<group_sid>`. Any token with the group *enabled* passes here:
//!      the broker, Explorer, ordinary processes.
//!   2. **PERMIT** (medium weight) — `ALE_USER_ID` matches an SD
//!      granting `<user_sid>` AND `IP_REMOTE_ADDRESS` is loopback
//!      (`127.0.0.0/8` v4, `::1` v6). Lets the sandboxed child reach
//!      the host-side proxy regardless of which ephemeral port it
//!      bound.
//!   3. **BLOCK** (low weight) — `ALE_USER_ID` matches `<user_sid>`.
//!      Catches the sandboxed child for everything else: its token has
//!      the group *deny-only*, so filter 1's AccessCheck fails for it.
//!
//! Filters carry a small JSON tag in `providerData` identifying the
//! `user_sid` and filter kind, so install/uninstall/status can locate
//! a specific user's filters by enumeration without relying on fixed
//! filter GUIDs (which would collide when multiple users on one
//! machine are fenced under the same sublayer).
//!
//! There is no marker file. `wfp status` enumerates the live engine;
//! `group status` queries SAM and the current token directly.

// The WFP structs are large and partially-initialised; the
// `..Default::default()` struct-update form clippy suggests is
// significantly less readable here than field-by-field assignment.
#![allow(clippy::field_reassign_with_default)]

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use windows::core::{GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    LocalFree, ERROR_MEMBER_IN_ALIAS, HANDLE, HLOCAL,
};
use windows::Win32::NetworkManagement::NetManagement::{
    NetLocalGroupAdd, NetLocalGroupAddMembers, NetLocalGroupDel,
    NERR_GroupExists, NERR_GroupNotFound, LOCALGROUP_INFO_1,
    LOCALGROUP_MEMBERS_INFO_0,
};
use windows::Win32::NetworkManagement::WindowsFilteringPlatform::{
    FwpmEngineClose0, FwpmEngineOpen0, FwpmFilterAdd0,
    FwpmFilterCreateEnumHandle0, FwpmFilterDeleteByKey0,
    FwpmFilterDestroyEnumHandle0, FwpmFilterEnum0, FwpmFreeMemory0,
    FwpmSubLayerAdd0, FwpmSubLayerDeleteByKey0, FwpmTransactionAbort0,
    FwpmTransactionBegin0, FwpmTransactionCommit0, FWPM_ACTION0,
    FWPM_ACTION0_0, FWPM_CONDITION_ALE_USER_ID,
    FWPM_CONDITION_IP_REMOTE_ADDRESS, FWPM_DISPLAY_DATA0, FWPM_FILTER0,
    FWPM_FILTER_CONDITION0, FWPM_FILTER_ENUM_TEMPLATE0,
    FWPM_FILTER_FLAG_PERSISTENT, FWPM_LAYER_ALE_AUTH_CONNECT_V4,
    FWPM_LAYER_ALE_AUTH_CONNECT_V6, FWPM_SUBLAYER0,
    FWPM_SUBLAYER_FLAG_PERSISTENT, FWP_ACTION_BLOCK, FWP_ACTION_PERMIT,
    FWP_ACTION_TYPE, FWP_BYTE_ARRAY16, FWP_BYTE_ARRAY16_TYPE, FWP_BYTE_BLOB,
    FWP_CONDITION_VALUE0, FWP_CONDITION_VALUE0_0,
    FWP_FILTER_ENUM_OVERLAPPING, FWP_MATCH_EQUAL,
    FWP_SECURITY_DESCRIPTOR_TYPE, FWP_UINT64, FWP_V4_ADDR_AND_MASK,
    FWP_V4_ADDR_MASK, FWP_VALUE0, FWP_VALUE0_0,
};
use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows::Win32::Security::{
    GetSecurityDescriptorLength, PSECURITY_DESCRIPTOR,
};

use crate::sid;
use crate::util::{from_pwstr, pcwstr, wstr};

/// Default group name. Overridable via `--name` so an embedding
/// product (or enterprise rollout) can pick its own.
pub const DEFAULT_GROUP_NAME: &str = "sandbox-runtime-net";
const GROUP_COMMENT: &str = "sandbox-runtime network sandbox membership";

/// Default sublayer GUID. Stable so uninstall can find filters from a
/// previous install. Overridable via `--sublayer-guid` so an
/// enterprise that provisions WFP via its own tooling can point us at
/// theirs. {2c5d0ad6-5f3b-4d4e-9b8f-1a3e7c9d0b21}
pub const DEFAULT_SUBLAYER_GUID: GUID =
    GUID::from_u128(0x2c5d0ad6_5f3b_4d4e_9b8f_1a3e7c9d0b21);

// WFP error codes we treat as benign idempotency outcomes.
const FWP_E_ALREADY_EXISTS: u32 = 0x80320009;
const FWP_E_FILTER_NOT_FOUND: u32 = 0x80320003;
const FWP_E_SUBLAYER_NOT_FOUND: u32 = 0x80320007;
const FWP_E_IN_USE: u32 = 0x8032000A;

const SDDL_REVISION_1: u32 = 1;

// ────────────────────── small RAII helpers ──────────────────────

/// Heap SD owned by us; freed via `LocalFree`.
struct OwnedSd {
    ptr: PSECURITY_DESCRIPTOR,
    len: u32,
}

impl OwnedSd {
    fn from_sddl(sddl: &str) -> Result<Self> {
        let w = wstr(sddl);
        let mut psd = PSECURITY_DESCRIPTOR::default();
        let mut sz: u32 = 0;
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                pcwstr(&w),
                SDDL_REVISION_1,
                &mut psd,
                Some(&mut sz),
            )
            .map_err(|e| {
                anyhow!(
                    "ConvertStringSecurityDescriptorToSecurityDescriptorW({sddl}): {e}"
                )
            })?;
            if sz == 0 {
                sz = GetSecurityDescriptorLength(psd);
            }
        }
        Ok(Self { ptr: psd, len: sz })
    }
    fn byte_blob(&self) -> FWP_BYTE_BLOB {
        FWP_BYTE_BLOB {
            size: self.len,
            data: self.ptr.0 as *mut u8,
        }
    }
}

impl Drop for OwnedSd {
    fn drop(&mut self) {
        if !self.ptr.0.is_null() {
            unsafe {
                let _ = LocalFree(HLOCAL(self.ptr.0));
            }
        }
    }
}

/// WFP engine handle; closed on drop.
struct EngineHandle(HANDLE);

impl EngineHandle {
    fn open() -> Result<Self> {
        let mut h = HANDLE::default();
        // RPC_C_AUTHN_DEFAULT
        let rc = unsafe {
            FwpmEngineOpen0(PCWSTR::null(), 0xFFFF_FFFF, None, None, &mut h)
        };
        if rc != 0 {
            return Err(anyhow!("FwpmEngineOpen0 failed: 0x{rc:08x}"));
        }
        Ok(Self(h))
    }
    fn h(&self) -> HANDLE {
        self.0
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = FwpmEngineClose0(self.0);
            }
        }
    }
}

// ────────────────────── condition builders ──────────────────────

fn fwp_uint64(slot: &mut u64) -> FWP_VALUE0 {
    FWP_VALUE0 {
        r#type: FWP_UINT64,
        Anonymous: FWP_VALUE0_0 {
            uint64: slot as *mut u64,
        },
    }
}

fn cond_sd(field_key: GUID, blob: &mut FWP_BYTE_BLOB) -> FWPM_FILTER_CONDITION0 {
    FWPM_FILTER_CONDITION0 {
        fieldKey: field_key,
        matchType: FWP_MATCH_EQUAL,
        conditionValue: FWP_CONDITION_VALUE0 {
            r#type: FWP_SECURITY_DESCRIPTOR_TYPE,
            Anonymous: FWP_CONDITION_VALUE0_0 {
                sd: blob as *mut _,
            },
        },
    }
}

fn cond_v4_subnet(
    field_key: GUID,
    am: &mut FWP_V4_ADDR_AND_MASK,
) -> FWPM_FILTER_CONDITION0 {
    FWPM_FILTER_CONDITION0 {
        fieldKey: field_key,
        matchType: FWP_MATCH_EQUAL,
        conditionValue: FWP_CONDITION_VALUE0 {
            r#type: FWP_V4_ADDR_MASK,
            Anonymous: FWP_CONDITION_VALUE0_0 {
                v4AddrMask: am as *mut _,
            },
        },
    }
}

fn cond_v6_addr(
    field_key: GUID,
    addr: &mut FWP_BYTE_ARRAY16,
) -> FWPM_FILTER_CONDITION0 {
    FWPM_FILTER_CONDITION0 {
        fieldKey: field_key,
        matchType: FWP_MATCH_EQUAL,
        conditionValue: FWP_CONDITION_VALUE0 {
            r#type: FWP_BYTE_ARRAY16_TYPE,
            Anonymous: FWP_CONDITION_VALUE0_0 {
                byteArray16: addr as *mut _,
            },
        },
    }
}

// ────────────────────── filter tagging ──────────────────────

/// JSON payload stored in each filter's `providerData` so we can
/// identify our own filters during enumerate/uninstall without fixed
/// filter GUIDs.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct FilterTag {
    /// Discriminator: `"srt-win"`. Anything else means the filter
    /// belongs to some other tool that happens to share our sublayer.
    pub tool: String,
    /// User SID this filter fences (string form).
    pub user_sid: String,
    /// One of `permit-group`, `permit-loopback`, `block-user`.
    pub kind: String,
}

impl FilterTag {
    fn new(user_sid: &str, kind: &str) -> Self {
        Self {
            tool: "srt-win".into(),
            user_sid: user_sid.into(),
            kind: kind.into(),
        }
    }
    fn to_blob_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("FilterTag is always serialisable")
    }
}

/// Summary of one filter under our sublayer (for `wfp status`).
#[derive(Debug, Serialize, Clone)]
pub struct FilterSummary {
    pub filter_key: String,
    pub layer: &'static str,
    pub action: &'static str,
    pub name: String,
    pub tag: Option<FilterTag>,
}

// ────────────────────── local group management ──────────────────────

/// Create the local group if it doesn't exist and add `user_sid` to
/// it. Idempotent.
pub fn ensure_group(name: &str, user_sid: &str) -> Result<()> {
    unsafe {
        let mut name_w = wstr(name);
        let mut comment_w = wstr(GROUP_COMMENT);
        let info = LOCALGROUP_INFO_1 {
            lgrpi1_name: PWSTR(name_w.as_mut_ptr()),
            lgrpi1_comment: PWSTR(comment_w.as_mut_ptr()),
        };
        let rc = NetLocalGroupAdd(
            PCWSTR::null(),
            1,
            &info as *const _ as *const u8,
            None,
        );
        // SAM returns ERROR_ALIAS_EXISTS (1379) for an existing local
        // group; some paths return NERR_GroupExists (2223). Either is
        // fine for idempotency.
        const ERROR_ALIAS_EXISTS: u32 = 1379;
        if rc != 0 && rc != NERR_GroupExists && rc != ERROR_ALIAS_EXISTS {
            return Err(anyhow!("NetLocalGroupAdd({name}): {rc}"));
        }
    }
    let psid = sid::LocalPsid::from_string(user_sid)?;
    unsafe {
        let name_w = wstr(name);
        let info = LOCALGROUP_MEMBERS_INFO_0 {
            lgrmi0_sid: psid.as_psid(),
        };
        let rc = NetLocalGroupAddMembers(
            PCWSTR::null(),
            pcwstr(&name_w),
            0,
            &info as *const _ as *const u8,
            1,
        );
        if rc != 0 && rc != ERROR_MEMBER_IN_ALIAS.0 {
            return Err(anyhow!(
                "NetLocalGroupAddMembers({name}, {user_sid}): {rc}"
            ));
        }
    }
    Ok(())
}

/// Delete the local group. Idempotent on `NERR_GroupNotFound`.
pub fn delete_group(name: &str) -> Result<()> {
    unsafe {
        let name_w = wstr(name);
        let rc = NetLocalGroupDel(PCWSTR::null(), pcwstr(&name_w));
        // 2220 (NERR_GroupNotFound) and 1376 (ERROR_NO_SUCH_ALIAS) both
        // mean "already gone" depending on Windows version.
        const ERROR_NO_SUCH_ALIAS: u32 = 1376;
        if rc != 0 && rc != NERR_GroupNotFound && rc != ERROR_NO_SUCH_ALIAS {
            return Err(anyhow!("NetLocalGroupDel({name}): {rc}"));
        }
    }
    Ok(())
}

// ────────────────────── filter enumeration ──────────────────────

const ALE_LAYERS: [(GUID, &str); 2] = [
    (FWPM_LAYER_ALE_AUTH_CONNECT_V4, "ale_auth_connect_v4"),
    (FWPM_LAYER_ALE_AUTH_CONNECT_V6, "ale_auth_connect_v6"),
];

/// Enumerate every filter in `sublayer` at the two ALE connect layers.
/// Returns summaries; `tag` is `Some` only for filters carrying a
/// parseable `srt-win` providerData tag.
pub fn enumerate_filters(sublayer: &GUID) -> Result<Vec<FilterSummary>> {
    let engine = EngineHandle::open()?;
    enumerate_in(&engine, sublayer)
}

fn enumerate_in(
    engine: &EngineHandle,
    sublayer: &GUID,
) -> Result<Vec<FilterSummary>> {
    let mut out = Vec::new();
    for (layer, layer_name) in ALE_LAYERS {
        let mut tmpl = FWPM_FILTER_ENUM_TEMPLATE0::default();
        tmpl.layerKey = layer;
        tmpl.enumType = FWP_FILTER_ENUM_OVERLAPPING;
        tmpl.actionMask = 0xFFFF_FFFF;
        let mut h = HANDLE::default();
        let rc = unsafe {
            FwpmFilterCreateEnumHandle0(engine.h(), Some(&tmpl), &mut h)
        };
        if rc != 0 {
            return Err(anyhow!(
                "FwpmFilterCreateEnumHandle0({layer_name}): 0x{rc:08x}"
            ));
        }
        loop {
            let mut entries: *mut *mut FWPM_FILTER0 = std::ptr::null_mut();
            let mut n: u32 = 0;
            let rc = unsafe {
                FwpmFilterEnum0(engine.h(), h, 256, &mut entries, &mut n)
            };
            if rc != 0 {
                unsafe {
                    let _ = FwpmFilterDestroyEnumHandle0(engine.h(), h);
                }
                return Err(anyhow!("FwpmFilterEnum0: 0x{rc:08x}"));
            }
            if n == 0 {
                if !entries.is_null() {
                    unsafe {
                        FwpmFreeMemory0(&mut (entries as *mut c_void));
                    }
                }
                break;
            }
            let slice =
                unsafe { std::slice::from_raw_parts(entries, n as usize) };
            for &fp in slice {
                if fp.is_null() {
                    continue;
                }
                let f = unsafe { &*fp };
                if &f.subLayerKey != sublayer {
                    continue;
                }
                let action = if f.action.r#type == FWP_ACTION_PERMIT {
                    "permit"
                } else if f.action.r#type == FWP_ACTION_BLOCK {
                    "block"
                } else {
                    "other"
                };
                let tag = if f.providerData.size > 0
                    && !f.providerData.data.is_null()
                {
                    let bytes = unsafe {
                        std::slice::from_raw_parts(
                            f.providerData.data,
                            f.providerData.size as usize,
                        )
                    };
                    serde_json::from_slice::<FilterTag>(bytes)
                        .ok()
                        .filter(|t| t.tool == "srt-win")
                } else {
                    None
                };
                out.push(FilterSummary {
                    filter_key: format!("{:?}", f.filterKey),
                    layer: layer_name,
                    action,
                    name: from_pwstr(f.displayData.name),
                    tag,
                });
            }
            unsafe {
                FwpmFreeMemory0(&mut (entries as *mut c_void));
            }
            if (n as usize) < 256 {
                break;
            }
        }
        unsafe {
            let _ = FwpmFilterDestroyEnumHandle0(engine.h(), h);
        }
    }
    Ok(out)
}

/// Delete every filter under `sublayer` whose tag's `user_sid` matches
/// (or all srt-win-tagged filters if `user_sid` is `None`). Returns the
/// number deleted. Does not delete the sublayer itself.
fn delete_tagged_filters(
    engine: &EngineHandle,
    sublayer: &GUID,
    user_sid: Option<&str>,
) -> Result<usize> {
    // Re-enumerate to get filterKey GUIDs (the summary stringifies
    // them; here we need the raw GUID so walk again).
    let mut deleted = 0usize;
    for (layer, _name) in ALE_LAYERS {
        let mut tmpl = FWPM_FILTER_ENUM_TEMPLATE0::default();
        tmpl.layerKey = layer;
        tmpl.enumType = FWP_FILTER_ENUM_OVERLAPPING;
        tmpl.actionMask = 0xFFFF_FFFF;
        let mut h = HANDLE::default();
        let rc = unsafe {
            FwpmFilterCreateEnumHandle0(engine.h(), Some(&tmpl), &mut h)
        };
        if rc != 0 {
            return Err(anyhow!(
                "FwpmFilterCreateEnumHandle0: 0x{rc:08x}"
            ));
        }
        let mut to_delete: Vec<GUID> = Vec::new();
        loop {
            let mut entries: *mut *mut FWPM_FILTER0 = std::ptr::null_mut();
            let mut n: u32 = 0;
            let rc = unsafe {
                FwpmFilterEnum0(engine.h(), h, 256, &mut entries, &mut n)
            };
            if rc != 0 {
                // Don't silently break: inside install_filters' txn,
                // a swallowed enum error would skip stale-filter
                // cleanup and the fresh six would be added on top,
                // growing the set every install.
                unsafe {
                    let _ = FwpmFilterDestroyEnumHandle0(engine.h(), h);
                }
                return Err(anyhow!("FwpmFilterEnum0: 0x{rc:08x}"));
            }
            if n == 0 {
                if !entries.is_null() {
                    unsafe {
                        FwpmFreeMemory0(&mut (entries as *mut c_void));
                    }
                }
                break;
            }
            let slice =
                unsafe { std::slice::from_raw_parts(entries, n as usize) };
            for &fp in slice {
                if fp.is_null() {
                    continue;
                }
                let f = unsafe { &*fp };
                if &f.subLayerKey != sublayer {
                    continue;
                }
                let matches = if f.providerData.size > 0
                    && !f.providerData.data.is_null()
                {
                    let bytes = unsafe {
                        std::slice::from_raw_parts(
                            f.providerData.data,
                            f.providerData.size as usize,
                        )
                    };
                    serde_json::from_slice::<FilterTag>(bytes)
                        .ok()
                        .filter(|t| t.tool == "srt-win")
                        .map(|t| match user_sid {
                            Some(u) => t.user_sid == u,
                            None => true,
                        })
                        .unwrap_or(false)
                } else {
                    false
                };
                if matches {
                    to_delete.push(f.filterKey);
                }
            }
            unsafe {
                FwpmFreeMemory0(&mut (entries as *mut c_void));
            }
            if (n as usize) < 256 {
                break;
            }
        }
        unsafe {
            let _ = FwpmFilterDestroyEnumHandle0(engine.h(), h);
        }
        for key in to_delete {
            let rc = unsafe { FwpmFilterDeleteByKey0(engine.h(), &key) };
            if rc == 0 {
                deleted += 1;
            } else if rc != FWP_E_FILTER_NOT_FOUND {
                return Err(anyhow!(
                    "FwpmFilterDeleteByKey0({key:?}): 0x{rc:08x}"
                ));
            }
        }
    }
    Ok(deleted)
}

// ────────────────────── install / uninstall ──────────────────────

#[allow(clippy::too_many_arguments)]
fn add_filter(
    engine: HANDLE,
    sublayer: &GUID,
    layer: GUID,
    name: &str,
    weight: u64,
    action: FWP_ACTION_TYPE,
    conditions: &mut [FWPM_FILTER_CONDITION0],
    tag_bytes: &mut [u8],
) -> Result<()> {
    let mut name_w = wstr(name);
    let mut desc_w = wstr("sandbox-runtime WFP filter");
    let mut weight_slot = weight;
    let mut filter = FWPM_FILTER0::default();
    // filterKey left zeroed → WFP assigns a fresh GUID. We identify
    // our filters via providerData, not by fixed key.
    filter.displayData = FWPM_DISPLAY_DATA0 {
        name: PWSTR(name_w.as_mut_ptr()),
        description: PWSTR(desc_w.as_mut_ptr()),
    };
    filter.flags = FWPM_FILTER_FLAG_PERSISTENT;
    filter.layerKey = layer;
    filter.subLayerKey = *sublayer;
    filter.weight = fwp_uint64(&mut weight_slot);
    filter.numFilterConditions = conditions.len() as u32;
    filter.filterCondition = if conditions.is_empty() {
        std::ptr::null_mut()
    } else {
        conditions.as_mut_ptr()
    };
    filter.action = FWPM_ACTION0 {
        r#type: action,
        Anonymous: FWPM_ACTION0_0 {
            filterType: GUID::zeroed(),
        },
    };
    filter.providerData = FWP_BYTE_BLOB {
        size: tag_bytes.len() as u32,
        data: tag_bytes.as_mut_ptr(),
    };
    let rc = unsafe {
        FwpmFilterAdd0(engine, &filter, PSECURITY_DESCRIPTOR::default(), None)
    };
    if rc != 0 && rc != FWP_E_ALREADY_EXISTS {
        return Err(anyhow!("FwpmFilterAdd0({name}): 0x{rc:08x}"));
    }
    Ok(())
}

/// Install (or refresh) the six filters for `user_sid` under
/// `sublayer`, keyed on `group_sid`. Idempotent: any existing
/// srt-win-tagged filters for this user are deleted first, then a
/// fresh set is added, all inside one WFP transaction.
pub fn install_filters(
    sublayer: &GUID,
    group_sid: &str,
    user_sid: &str,
) -> Result<()> {
    let sd_group = OwnedSd::from_sddl(&format!("O:LSD:(A;;CC;;;{group_sid})"))
        .context("build group SD")?;
    let sd_user = OwnedSd::from_sddl(&format!("O:LSD:(A;;CC;;;{user_sid})"))
        .context("build user SD")?;

    let engine = EngineHandle::open()?;
    let rc = unsafe { FwpmTransactionBegin0(engine.h(), 0) };
    if rc != 0 {
        return Err(anyhow!("FwpmTransactionBegin0: 0x{rc:08x}"));
    }

    let result: Result<()> = (|| {
        // Sublayer (idempotent). The display name identifies the
        // owning tool, not the group — one sublayer may carry filters
        // for several groups/users.
        let mut sl_name = wstr("srt-win");
        let mut sl_desc =
            wstr("sandbox-runtime WFP sublayer (deny-only-group fence)");
        let sl = FWPM_SUBLAYER0 {
            subLayerKey: *sublayer,
            displayData: FWPM_DISPLAY_DATA0 {
                name: PWSTR(sl_name.as_mut_ptr()),
                description: PWSTR(sl_desc.as_mut_ptr()),
            },
            flags: FWPM_SUBLAYER_FLAG_PERSISTENT,
            providerKey: std::ptr::null_mut(),
            providerData: FWP_BYTE_BLOB {
                size: 0,
                data: std::ptr::null_mut(),
            },
            weight: 0x8000,
        };
        let rc = unsafe {
            FwpmSubLayerAdd0(
                engine.h(),
                &sl,
                PSECURITY_DESCRIPTOR::default(),
            )
        };
        if rc != 0 && rc != FWP_E_ALREADY_EXISTS {
            return Err(anyhow!("FwpmSubLayerAdd0: 0x{rc:08x}"));
        }

        // Idempotency: drop any stale filters for this user before
        // re-adding. (Inside the transaction so a crash leaves the
        // previous state intact.)
        let _ = delete_tagged_filters(&engine, sublayer, Some(user_sid))?;

        // Weights — kept below 2^60 so we stay in WFP's "manual
        // weight" class (top 4 bits are auto-classifier).
        const W_HIGH: u64 = 0x0F00_0000_0000_0000;
        const W_MED: u64 = 0x0C00_0000_0000_0000;
        const W_LOW: u64 = 0x0400_0000_0000_0000;

        let mut sd_group_blob = sd_group.byte_blob();
        let mut sd_user_blob = sd_user.byte_blob();

        // 127.0.0.0/8
        let mut v4_loop = FWP_V4_ADDR_AND_MASK {
            addr: 0x7F00_0000,
            mask: 0xFF00_0000,
        };
        // ::1
        let mut v6_loop = FWP_BYTE_ARRAY16 {
            byteArray16: [0; 16],
        };
        v6_loop.byteArray16[15] = 1;

        let mut tag_pg = FilterTag::new(user_sid, "permit-group").to_blob_bytes();
        let mut tag_pl = FilterTag::new(user_sid, "permit-loopback").to_blob_bytes();
        let mut tag_bu = FilterTag::new(user_sid, "block-user").to_blob_bytes();

        // ── IPv4 ──
        let mut c1 =
            [cond_sd(FWPM_CONDITION_ALE_USER_ID, &mut sd_group_blob)];
        add_filter(
            engine.h(),
            sublayer,
            FWPM_LAYER_ALE_AUTH_CONNECT_V4,
            "srt-win-v4-permit-group",
            W_HIGH,
            FWP_ACTION_PERMIT,
            &mut c1,
            &mut tag_pg,
        )?;
        let mut c2 = [
            cond_sd(FWPM_CONDITION_ALE_USER_ID, &mut sd_user_blob),
            cond_v4_subnet(FWPM_CONDITION_IP_REMOTE_ADDRESS, &mut v4_loop),
        ];
        add_filter(
            engine.h(),
            sublayer,
            FWPM_LAYER_ALE_AUTH_CONNECT_V4,
            "srt-win-v4-permit-loopback",
            W_MED,
            FWP_ACTION_PERMIT,
            &mut c2,
            &mut tag_pl,
        )?;
        let mut c3 =
            [cond_sd(FWPM_CONDITION_ALE_USER_ID, &mut sd_user_blob)];
        add_filter(
            engine.h(),
            sublayer,
            FWPM_LAYER_ALE_AUTH_CONNECT_V4,
            "srt-win-v4-block-user",
            W_LOW,
            FWP_ACTION_BLOCK,
            &mut c3,
            &mut tag_bu,
        )?;

        // ── IPv6 ──
        let mut c4 =
            [cond_sd(FWPM_CONDITION_ALE_USER_ID, &mut sd_group_blob)];
        add_filter(
            engine.h(),
            sublayer,
            FWPM_LAYER_ALE_AUTH_CONNECT_V6,
            "srt-win-v6-permit-group",
            W_HIGH,
            FWP_ACTION_PERMIT,
            &mut c4,
            &mut tag_pg,
        )?;
        let mut c5 = [
            cond_sd(FWPM_CONDITION_ALE_USER_ID, &mut sd_user_blob),
            cond_v6_addr(FWPM_CONDITION_IP_REMOTE_ADDRESS, &mut v6_loop),
        ];
        add_filter(
            engine.h(),
            sublayer,
            FWPM_LAYER_ALE_AUTH_CONNECT_V6,
            "srt-win-v6-permit-loopback",
            W_MED,
            FWP_ACTION_PERMIT,
            &mut c5,
            &mut tag_pl,
        )?;
        let mut c6 =
            [cond_sd(FWPM_CONDITION_ALE_USER_ID, &mut sd_user_blob)];
        add_filter(
            engine.h(),
            sublayer,
            FWPM_LAYER_ALE_AUTH_CONNECT_V6,
            "srt-win-v6-block-user",
            W_LOW,
            FWP_ACTION_BLOCK,
            &mut c6,
            &mut tag_bu,
        )?;

        Ok(())
    })();

    if let Err(e) = result {
        unsafe {
            let _ = FwpmTransactionAbort0(engine.h());
        }
        return Err(e);
    }
    let rc = unsafe { FwpmTransactionCommit0(engine.h()) };
    if rc != 0 {
        return Err(anyhow!("FwpmTransactionCommit0: 0x{rc:08x}"));
    }
    Ok(())
}

/// Remove the filters for `user_sid` (or all srt-win filters if `None`)
/// under `sublayer`. If the sublayer ends up empty, attempt to delete
/// it too (best-effort; another user's filters may keep it busy).
pub fn uninstall_filters(
    sublayer: &GUID,
    user_sid: Option<&str>,
) -> Result<usize> {
    let engine = EngineHandle::open()?;
    let rc = unsafe { FwpmTransactionBegin0(engine.h(), 0) };
    if rc != 0 {
        return Err(anyhow!("FwpmTransactionBegin0: 0x{rc:08x}"));
    }
    let n = match delete_tagged_filters(&engine, sublayer, user_sid) {
        Ok(n) => n,
        Err(e) => {
            unsafe {
                let _ = FwpmTransactionAbort0(engine.h());
            }
            return Err(e);
        }
    };
    // Try to delete the sublayer; FWP_E_IN_USE means another user's
    // filters are still under it — fine.
    let rc = unsafe { FwpmSubLayerDeleteByKey0(engine.h(), sublayer) };
    if rc != 0
        && rc != FWP_E_SUBLAYER_NOT_FOUND
        && rc != FWP_E_FILTER_NOT_FOUND
        && rc != FWP_E_IN_USE
    {
        unsafe {
            let _ = FwpmTransactionAbort0(engine.h());
        }
        return Err(anyhow!("FwpmSubLayerDeleteByKey0: 0x{rc:08x}"));
    }
    let rc = unsafe { FwpmTransactionCommit0(engine.h()) };
    if rc != 0 {
        return Err(anyhow!("FwpmTransactionCommit0: 0x{rc:08x}"));
    }
    Ok(n)
}

/// Status of the WFP fence for `user_sid`. `installed` iff at least
/// one `block-user` and one `permit-group` srt-win filter for that
/// user exist under `sublayer`. (We don't insist on an exact count so
/// enterprise tooling that adds extras under the same sublayer
/// doesn't break detection.)
#[derive(Debug, Serialize)]
pub struct WfpStatus {
    pub state: &'static str,
    pub filters: usize,
}

pub fn filter_status(sublayer: &GUID, user_sid: &str) -> Result<WfpStatus> {
    let all = enumerate_filters(sublayer)?;
    let mine: Vec<_> = all
        .iter()
        .filter(|f| {
            f.tag
                .as_ref()
                .map(|t| t.user_sid == user_sid)
                .unwrap_or(false)
        })
        .collect();
    let has_block = mine
        .iter()
        .any(|f| f.tag.as_ref().map(|t| t.kind == "block-user").unwrap_or(false));
    let has_permit_group = mine.iter().any(|f| {
        f.tag
            .as_ref()
            .map(|t| t.kind == "permit-group")
            .unwrap_or(false)
    });
    let state = if has_block && has_permit_group {
        "installed"
    } else {
        "absent"
    };
    Ok(WfpStatus {
        state,
        filters: mine.len(),
    })
}

/// Parse a `--sublayer-guid` argument. Accepts braced or unbraced
/// canonical form. The `windows` crate only offers a panicking
/// `From<&str>` for `GUID`, so validate the shape ourselves first.
pub fn parse_guid(s: &str) -> Result<GUID> {
    let t = s.trim().trim_start_matches('{').trim_end_matches('}');
    // 8-4-4-4-12 hex with hyphens, exactly 36 chars.
    let ok = t.len() == 36
        && t.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        });
    if !ok {
        return Err(anyhow!(
            "invalid GUID '{s}': expected xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
        ));
    }
    Ok(GUID::from(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SDDL templates used by `install_filters` must parse for
    /// representative SIDs. Catches template typos without needing a
    /// live WFP engine.
    #[test]
    fn sddl_templates_parse() {
        for sid in ["S-1-5-32-545", "S-1-5-18"] {
            let sd = OwnedSd::from_sddl(&format!("O:LSD:(A;;CC;;;{sid})"))
                .expect("sddl");
            assert!(!sd.ptr.0.is_null());
            assert!(sd.len > 0);
        }
    }

    #[test]
    fn sddl_rejects_garbage() {
        assert!(OwnedSd::from_sddl("O:LSD:(A;;CC;;;NOT-A-SID)").is_err());
    }

    #[test]
    fn filter_tag_round_trip() {
        let t = FilterTag::new("S-1-5-21-1-2-3-1000", "block-user");
        let bytes = t.to_blob_bytes();
        let back: FilterTag = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn parse_guid_accepts_both_forms() {
        let g1 =
            parse_guid("2c5d0ad6-5f3b-4d4e-9b8f-1a3e7c9d0b21").unwrap();
        let g2 =
            parse_guid("{2c5d0ad6-5f3b-4d4e-9b8f-1a3e7c9d0b21}").unwrap();
        assert_eq!(g1, g2);
        assert_eq!(g1, DEFAULT_SUBLAYER_GUID);
    }

    #[test]
    fn parse_guid_rejects_garbage() {
        assert!(parse_guid("not-a-guid").is_err());
    }
}
