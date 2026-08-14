//! Late-bound Outlook.Application COM (classic Outlook only).

#![cfg(windows)]

use std::time::{Duration, Instant};

use windows::core::{Interface, BSTR, GUID, PCWSTR};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, CLSIDFromProgID, COINIT_APARTMENTTHREADED, DISPATCH_FLAGS,
    DISPATCH_METHOD, DISPATCH_PROPERTYGET, DISPPARAMS, IDispatch, EXCEPINFO,
};
use windows::Win32::System::Ole::GetActiveObject;
use windows::Win32::System::Variant::{
    VariantChangeType, VariantClear, VariantInit, VAR_CHANGE_FLAGS, VARIANT, VT_BSTR, VT_BYREF,
    VT_CY, VT_DATE, VT_EMPTY, VT_I2, VT_I4, VT_I8, VT_NULL, VT_R4, VT_R8, VT_UI2, VT_UI4,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWMINNOACTIVE;

use crate::mail::sync::{normalize_mail_body, OutlookFolderInfo, OutlookMessage};

/// Result of connecting to classic Outlook via COM.
#[derive(Debug, Clone, Copy)]
pub struct OutlookConnectInfo {
    /// True when this call started Outlook (was not already running).
    pub launched: bool,
}

/// Must be called on an STA thread. Pair with `com_uninit`.
pub fn com_init() -> Result<(), String> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|e| format!("CoInitializeEx failed: {e}"))
    }
}

pub fn com_uninit() {
    unsafe {
        CoUninitialize();
    }
}

/// True when an Outlook.Application is already registered in the ROT.
pub fn outlook_is_running() -> bool {
    get_active_outlook().is_ok()
}

pub fn detect_outlook() -> Result<String, String> {
    let (app, _) = connect_outlook(true)?;
    let version = get_string_prop(&app, "Version").unwrap_or_else(|_| "unknown".into());
    Ok(format!("Outlook {version}"))
}

pub fn list_mail_folders() -> Result<Vec<OutlookFolderInfo>, String> {
    let (app, _) = connect_outlook(true)?;
    let session = get_session(&app)?;
    let stores = get_dispatch_prop(&session, "Stores")?;
    let store_count = get_i32_prop(&stores, "Count").unwrap_or(0);
    let mut out = Vec::new();
    for i in 1..=store_count {
        let store = match get_item_dispatch(&stores, i) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let store_id = get_string_prop(&store, "StoreID").unwrap_or_default();
        if store_id.is_empty() {
            continue;
        }
        let store_name = get_string_prop(&store, "DisplayName").unwrap_or_else(|_| "Store".into());
        let root = match invoke_method0(&store, "GetRootFolder").and_then(|v| dispatch_from_variant(&v))
        {
            Ok(r) => r,
            Err(_) => continue,
        };
        collect_folders(&root, &store_id, &store_name, &mut out, 0)?;
    }
    Ok(out)
}

fn get_session(app: &IDispatch) -> Result<IDispatch, String> {
    if let Ok(s) = get_dispatch_prop(app, "Session") {
        return Ok(s);
    }
    let arg = bstr_variant("MAPI");
    let result = invoke(app, "GetNamespace", DISPATCH_METHOD, &[arg])?;
    dispatch_from_variant(&result)
}

fn collect_folders(
    folder: &IDispatch,
    store_id: &str,
    parent_label: &str,
    out: &mut Vec<OutlookFolderInfo>,
    depth: usize,
) -> Result<(), String> {
    if depth > 12 {
        return Ok(());
    }
    let name = get_string_prop(folder, "Name").unwrap_or_else(|_| "Folder".into());
    let entry_id = get_string_prop(folder, "EntryID").unwrap_or_default();
    // Outlook often names the store root the same as the account (e.g. email).
    // Avoid `abc@x / abc@x / Inbox` → prefer `abc@x / Inbox`.
    let path_label = if name.eq_ignore_ascii_case(parent_label) {
        name.clone()
    } else {
        format!("{parent_label} / {name}")
    };
    let default_type = get_i32_prop(folder, "DefaultItemType").unwrap_or(0);
    let item_count = get_dispatch_prop(folder, "Items")
        .ok()
        .and_then(|items| get_i32_prop(&items, "Count").ok())
        .unwrap_or(0);

    if !entry_id.is_empty() && (default_type == 0 || item_count > 0) {
        out.push(OutlookFolderInfo {
            store_id: store_id.to_string(),
            entry_id,
            name: name.clone(),
            path_label: path_label.clone(),
            item_count,
        });
    }

    let folders = match get_dispatch_prop(folder, "Folders") {
        Ok(f) => f,
        Err(_) => return Ok(()),
    };
    let count = get_i32_prop(&folders, "Count").unwrap_or(0);
    for i in 1..=count {
        if let Ok(child) = get_item_dispatch(&folders, i) {
            let _ = collect_folders(&child, store_id, &path_label, out, depth + 1);
        }
    }
    Ok(())
}

pub fn fetch_messages_in_folder(
    folder_entry_id: &str,
    store_id: &str,
    since_unix: i64,
    allow_launch: bool,
) -> Result<Vec<OutlookMessage>, String> {
    let (app, _) = connect_outlook(allow_launch)?;
    let session = get_session(&app)?;
    let folder = get_folder_from_id(&session, folder_entry_id)?;
    let folder_name = get_string_prop(&folder, "Name").unwrap_or_else(|_| "Folder".into());
    let items = get_dispatch_prop(&folder, "Items")?;
    let _ = invoke_method1_str(&items, "Sort", "[ReceivedTime]", true);

    let restricted = if since_unix > 0 {
        let filter = outlook_date_filter(since_unix);
        match invoke_method1_str(&items, "Restrict", &filter, false) {
            Ok(v) => dispatch_from_variant(&v).unwrap_or(items),
            Err(_) => items,
        }
    } else {
        items
    };

    let count = get_i32_prop(&restricted, "Count").unwrap_or(0);
    let mut out = Vec::new();
    let mut missing_dates = 0u32;
    for i in 1..=count {
        let item = match get_item_dispatch(&restricted, i) {
            Ok(it) => it,
            Err(_) => continue,
        };
        let class = get_i32_prop(&item, "Class").unwrap_or(0);
        if class != 0 && class != 43 {
            continue;
        }
        let entry_id = match get_string_prop(&item, "EntryID") {
            Ok(e) if !e.is_empty() => e,
            _ => continue,
        };
        let subject = get_string_prop(&item, "Subject").unwrap_or_default();
        let plain = get_string_prop(&item, "Body").unwrap_or_default();
        let html_body = get_string_prop(&item, "HTMLBody").unwrap_or_default();
        let body_text = normalize_mail_body(&plain, &html_body);
        let from = get_string_prop(&item, "SenderName")
            .or_else(|_| get_string_prop(&item, "SentOnBehalfOfName"))
            .unwrap_or_default();
        let conversation_id = get_string_prop(&item, "ConversationID").unwrap_or_default();
        let received_unix = get_date_unix(&item, "ReceivedTime")
            .or_else(|_| get_date_unix(&item, "SentOn"))
            .unwrap_or(0);
        let last_mod_unix = get_date_unix(&item, "LastModificationTime").unwrap_or(received_unix);
        if received_unix <= 0 {
            missing_dates += 1;
        }

        if since_unix > 0 && received_unix > 0 && received_unix < since_unix {
            continue;
        }

        let _ = last_mod_unix;
        out.push(OutlookMessage {
            store_id: store_id.to_string(),
            entry_id,
            subject,
            body_text,
            from,
            conversation_id,
            folder_name: folder_name.clone(),
            received_unix,
            last_mod_unix,
        });
    }
    if missing_dates > 0 {
        eprintln!(
            "argos: {missing_dates}/{count} messages in '{folder_name}' had no usable ReceivedTime/SentOn"
        );
    }
    Ok(out)
}

pub fn open_mail_item(store_id: &str, entry_id: &str) -> Result<(), String> {
    let (app, _) = connect_outlook(true)?;
    let session = get_session(&app)?;
    let item = get_item_from_id(&session, entry_id, Some(store_id)).or_else(|with_store| {
        get_item_from_id(&session, entry_id, None).map_err(|without_store| {
            format!("メールを取得できません（{with_store} / {without_store}）")
        })
    })?;

    display_mail_item(&item)?;
    activate_inspector(&item);
    activate_outlook_window(&app);
    Ok(())
}

fn display_mail_item(item: &IDispatch) -> Result<(), String> {
    invoke_method0(item, "Display")
        .or_else(|_| invoke(item, "Display", DISPATCH_METHOD, &[bool_variant(false)]))
        .map(|_| ())
        .map_err(|e| format!("メールを表示できません: {e}"))
}

fn activate_inspector(item: &IDispatch) {
    let inspector = get_dispatch_prop(item, "GetInspector").or_else(|_| {
        invoke_method0(item, "GetInspector").and_then(|v| dispatch_from_variant(&v))
    });
    let Ok(insp) = inspector else {
        return;
    };
    let _ = invoke_method0(&insp, "Activate");
}

fn activate_outlook_window(app: &IDispatch) {
    if let Ok(win) = get_dispatch_prop(app, "ActiveWindow") {
        let _ = invoke_method0(&win, "Activate");
        return;
    }
    if let Ok(v) = invoke_method0(app, "ActiveWindow") {
        if let Ok(win) = dispatch_from_variant(&v) {
            let _ = invoke_method0(&win, "Activate");
        }
    }
}

/// Connect to classic Outlook. Prefer a running instance; optionally launch normally
/// (never via CoCreate / -Embedding). Does not force the main window to the foreground.
pub fn connect_outlook(allow_launch: bool) -> Result<(IDispatch, OutlookConnectInfo), String> {
    if let Ok(app) = get_active_outlook() {
        return Ok((app, OutlookConnectInfo { launched: false }));
    }
    if !allow_launch {
        return Err("Outlook が起動していないためスキップしました".into());
    }
    launch_outlook_background()?;
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(400));
        if let Ok(app) = get_active_outlook() {
            return Ok((app, OutlookConnectInfo { launched: true }));
        }
    }
    Err(
        "Outlook の起動を確認できませんでした。クラシック Outlook を開いてから再試行してください。"
            .into(),
    )
}

fn get_active_outlook() -> Result<IDispatch, String> {
    unsafe {
        let clsid = CLSIDFromProgID(windows::core::w!("Outlook.Application")).map_err(|e| {
            format!(
                "Outlook クラシックが見つかりません（{e}）。新しい Outlook のみの環境では利用できません。"
            )
        })?;
        let mut unk = None;
        GetActiveObject(&clsid, None, &mut unk).map_err(|_| {
            "Outlook は起動していません".to_string()
        })?;
        let unk = unk.ok_or_else(|| "Outlook は起動していません".to_string())?;
        unk.cast::<IDispatch>()
            .map_err(|e| format!("Outlook IDispatch: {e}"))
    }
}

fn launch_outlook_background() -> Result<(), String> {
    unsafe {
        // Normal process (not -Embedding); do not activate / bring to foreground.
        let ret = ShellExecuteW(
            None,
            windows::core::w!("open"),
            windows::core::w!("outlook.exe"),
            None,
            None,
            SW_SHOWMINNOACTIVE,
        );
        if ret.0 as isize <= 32 {
            return Err(format!(
                "Outlook を起動できません（ShellExecute={}). クラシック Outlook がインストールされているか確認してください。",
                ret.0 as isize
            ));
        }
    }
    Ok(())
}

fn get_folder_from_id(session: &IDispatch, entry_id: &str) -> Result<IDispatch, String> {
    let v = invoke_method1_str(session, "GetFolderFromID", entry_id, false)?;
    dispatch_from_variant(&v)
}

fn get_item_from_id(
    session: &IDispatch,
    entry_id: &str,
    store_id: Option<&str>,
) -> Result<IDispatch, String> {
    let mut args = vec![bstr_variant(entry_id)];
    if let Some(sid) = store_id.filter(|s| !s.is_empty()) {
        args.push(bstr_variant(sid));
    }
    // IDispatch args are in reverse order
    args.reverse();
    let v = invoke(session, "GetItemFromID", DISPATCH_METHOD, &args)?;
    dispatch_from_variant(&v)
}

fn outlook_date_filter(since_unix: i64) -> String {
    let dt = chrono::DateTime::from_timestamp(since_unix, 0)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).unwrap());
    let s = dt.format("%Y-%m-%d %H:%M").to_string();
    format!("[ReceivedTime] >= '{s}'")
}

fn get_dispatch_prop(obj: &IDispatch, name: &str) -> Result<IDispatch, String> {
    let v = invoke(obj, name, DISPATCH_PROPERTYGET, &[])?;
    dispatch_from_variant(&v)
}

fn get_string_prop(obj: &IDispatch, name: &str) -> Result<String, String> {
    let v = invoke(obj, name, DISPATCH_PROPERTYGET, &[])?;
    string_from_variant(&v)
}

fn get_i32_prop(obj: &IDispatch, name: &str) -> Result<i32, String> {
    let v = invoke(obj, name, DISPATCH_PROPERTYGET, &[])?;
    i32_from_variant(&v)
}

fn get_date_unix(obj: &IDispatch, name: &str) -> Result<i64, String> {
    let v = invoke(obj, name, DISPATCH_PROPERTYGET, &[])?;
    match ole_date_to_unix(&v) {
        Ok(n) if n > 0 => return Ok(n),
        _ => {}
    }
    get_mapi_date_unix(obj, name)
}

fn mapi_date_schema(name: &str) -> Option<&'static str> {
    match name {
        "ReceivedTime" => Some("http://schemas.microsoft.com/mapi/proptag/0x0E060040"),
        "SentOn" => Some("http://schemas.microsoft.com/mapi/proptag/0x00390040"),
        "LastModificationTime" => Some("http://schemas.microsoft.com/mapi/proptag/0x30080040"),
        _ => None,
    }
}

fn get_mapi_date_unix(obj: &IDispatch, name: &str) -> Result<i64, String> {
    let schema = mapi_date_schema(name).ok_or_else(|| format!("no MAPI schema for {name}"))?;
    let pa = get_dispatch_prop(obj, "PropertyAccessor")?;
    let v = invoke(
        &pa,
        "GetProperty",
        DISPATCH_METHOD,
        &[bstr_variant(schema)],
    )?;
    let n = ole_date_to_unix(&v)?;
    if n <= 0 {
        Err(format!("empty MAPI date ({name})"))
    } else {
        Ok(n)
    }
}

fn get_item_dispatch(collection: &IDispatch, index: i32) -> Result<IDispatch, String> {
    let arg = i32_variant(index);
    let v = invoke(collection, "Item", DISPATCH_METHOD, &[arg])
        .or_else(|_| invoke(collection, "Item", DISPATCH_PROPERTYGET, &[i32_variant(index)]))?;
    dispatch_from_variant(&v)
}

fn invoke_method0(obj: &IDispatch, name: &str) -> Result<VARIANT, String> {
    invoke(obj, name, DISPATCH_METHOD, &[])
}

fn invoke_method1_str(
    obj: &IDispatch,
    name: &str,
    arg: &str,
    descending_sort: bool,
) -> Result<VARIANT, String> {
    if name == "Sort" {
        let a0 = bool_variant(descending_sort);
        let a1 = bstr_variant(arg);
        return invoke(obj, name, DISPATCH_METHOD, &[a0, a1]);
    }
    let a = bstr_variant(arg);
    invoke(obj, name, DISPATCH_METHOD, &[a])
}

fn invoke(
    obj: &IDispatch,
    name: &str,
    flags: DISPATCH_FLAGS,
    args: &[VARIANT],
) -> Result<VARIANT, String> {
    unsafe {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut dispid = 0i32;
        let name_ptr = PCWSTR::from_raw(wide.as_ptr());
        obj.GetIDsOfNames(&GUID::zeroed(), &name_ptr, 1, 0, &mut dispid)
            .map_err(|e| format!("GetIDsOfNames({name}): {e}"))?;

        let mut args_owned: Vec<VARIANT> = args.to_vec();
        let mut params = DISPPARAMS::default();
        if !args_owned.is_empty() {
            params.cArgs = args_owned.len() as u32;
            params.rgvarg = args_owned.as_mut_ptr();
        }
        let mut result = VariantInit();
        let mut excep = EXCEPINFO::default();
        let mut arg_err = 0u32;
        let hr = obj.Invoke(
            dispid,
            &GUID::zeroed(),
            0,
            flags,
            &params,
            Some(&mut result),
            Some(&mut excep),
            Some(&mut arg_err),
        );
        for a in &mut args_owned {
            let _ = VariantClear(a);
        }
        hr.map_err(|e| {
            let desc = if !excep.bstrDescription.is_empty() {
                excep.bstrDescription.to_string()
            } else {
                e.to_string()
            };
            format!("Invoke({name}): {desc}")
        })?;
        Ok(result)
    }
}

fn dispatch_from_variant(v: &VARIANT) -> Result<IDispatch, String> {
    IDispatch::try_from(v).map_err(|e| format!("expected IDispatch: {e}"))
}

fn string_from_variant(v: &VARIANT) -> Result<String, String> {
    let vt = v.vt();
    if vt == VT_EMPTY || vt == VT_NULL {
        return Ok(String::new());
    }
    let b = BSTR::try_from(v).map_err(|e| format!("expected string VARIANT: {e}"))?;
    Ok(b.to_string())
}

fn i32_from_variant(v: &VARIANT) -> Result<i32, String> {
    i32::try_from(v).map_err(|e| format!("expected i32 VARIANT: {e}"))
}

fn ole_date_to_unix(v: &VARIANT) -> Result<i64, String> {
    let vt = v.vt();
    if vt == VT_EMPTY || vt == VT_NULL {
        return Err("empty date VARIANT".into());
    }
    if let Some(n) = unsafe { variant_numeric(v) } {
        let unix = crate::mail::ole_date::numeric_to_unix(n);
        if unix > 0 {
            return Ok(unix);
        }
    }
    if vt == VT_BSTR {
        if let Ok(s) = BSTR::try_from(v) {
            if let Some(unix) = crate::mail::ole_date::parse_outlook_date_string(&s.to_string()) {
                return Ok(unix);
            }
        }
    }
    if let Some(unix) = variant_change_to_unix(v, VT_R8, |dest| unsafe {
        variant_numeric(dest).map(crate::mail::ole_date::numeric_to_unix)
    }) {
        if unix > 0 {
            return Ok(unix);
        }
    }
    if let Some(unix) = variant_change_to_unix(v, VT_BSTR, |dest| {
        BSTR::try_from(dest)
            .ok()
            .and_then(|s| crate::mail::ole_date::parse_outlook_date_string(&s.to_string()))
    }) {
        if unix > 0 {
            return Ok(unix);
        }
    }
    Err(format!("expected date VARIANT (vt={})", vt.0))
}

fn variant_change_to_unix<F>(v: &VARIANT, vt: windows::Win32::System::Variant::VARENUM, f: F) -> Option<i64>
where
    F: FnOnce(&VARIANT) -> Option<i64>,
{
    unsafe {
        let mut dest = VariantInit();
        let ok = VariantChangeType(&mut dest, v, VAR_CHANGE_FLAGS(0), vt).is_ok();
        let out = if ok { f(&dest) } else { None };
        out
    }
}

unsafe fn variant_numeric(v: &VARIANT) -> Option<f64> {
    let rec = &*v.Anonymous.Anonymous;
    let vt = rec.vt;
    let byref = vt.contains(VT_BYREF);
    let base = windows::Win32::System::Variant::VARENUM(vt.0 & !VT_BYREF.0);
    let inner = &rec.Anonymous;
    if byref {
        return if base == VT_DATE || base == VT_R8 {
            let p = inner.pdate;
            if p.is_null() {
                None
            } else {
                Some(*p)
            }
        } else if base == VT_R4 {
            let p = inner.pfltVal;
            if p.is_null() {
                None
            } else {
                Some(*p as f64)
            }
        } else if base == VT_I4 {
            let p = inner.plVal;
            if p.is_null() {
                None
            } else {
                Some(*p as f64)
            }
        } else if base == VT_I8 {
            let p = inner.pllVal;
            if p.is_null() {
                None
            } else {
                Some(*p as f64)
            }
        } else if base == VT_CY {
            let p = inner.pcyVal;
            if p.is_null() {
                None
            } else {
                Some((*p).int64 as f64 / 10_000.0)
            }
        } else {
            None
        };
    }
    if base == VT_DATE || base == VT_R8 {
        Some(inner.date)
    } else if base == VT_R4 {
        Some(inner.fltVal as f64)
    } else if base == VT_I2 {
        Some(inner.iVal as f64)
    } else if base == VT_I4 {
        Some(inner.lVal as f64)
    } else if base == VT_I8 {
        Some(inner.llVal as f64)
    } else if base == VT_UI2 {
        Some(inner.uiVal as f64)
    } else if base == VT_UI4 {
        Some(inner.ulVal as f64)
    } else if base == VT_CY {
        Some(inner.cyVal.int64 as f64 / 10_000.0)
    } else {
        None
    }
}

fn bstr_variant(s: &str) -> VARIANT {
    VARIANT::from(BSTR::from(s))
}

fn i32_variant(n: i32) -> VARIANT {
    VARIANT::from(n)
}

fn bool_variant(b: bool) -> VARIANT {
    VARIANT::from(b)
}
