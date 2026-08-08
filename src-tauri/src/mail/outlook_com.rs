//! Late-bound Outlook.Application COM (classic Outlook only).

#![cfg(windows)]

use windows::core::{Interface, BSTR, GUID, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_LOCAL_SERVER, CLSIDFromProgID,
    COINIT_APARTMENTTHREADED, DISPATCH_FLAGS, DISPATCH_METHOD, DISPATCH_PROPERTYGET, DISPPARAMS,
    IDispatch, EXCEPINFO,
};
use windows::Win32::System::Variant::{
    VariantClear, VariantInit, VARIANT, VT_EMPTY, VT_NULL,
};

use crate::mail::sync::{normalize_mail_body, OutlookFolderInfo, OutlookMessage};

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

pub fn detect_outlook() -> Result<String, String> {
    let app = create_outlook()?;
    let version = get_string_prop(&app, "Version").unwrap_or_else(|_| "unknown".into());
    Ok(format!("Outlook {version}"))
}

pub fn list_mail_folders() -> Result<Vec<OutlookFolderInfo>, String> {
    let app = create_outlook()?;
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
) -> Result<Vec<OutlookMessage>, String> {
    let app = create_outlook()?;
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
    Ok(out)
}

pub fn open_mail_item(store_id: &str, entry_id: &str) -> Result<(), String> {
    let app = create_outlook()?;
    let session = get_session(&app)?;
    let item = get_item_from_id(&session, entry_id, Some(store_id))?;
    let _ = invoke_method0(&item, "Display")?;
    Ok(())
}

fn create_outlook() -> Result<IDispatch, String> {
    unsafe {
        let clsid = CLSIDFromProgID(windows::core::w!("Outlook.Application")).map_err(|e| {
            format!(
                "Outlook クラシックが見つかりません（{e}）。新しい Outlook のみの環境では利用できません。"
            )
        })?;
        let unk: windows::core::IUnknown =
            CoCreateInstance(&clsid, None, CLSCTX_LOCAL_SERVER).map_err(|e| {
                format!("Outlook を起動できません（{e}）。クラシック Outlook がインストールされているか確認してください。")
            })?;
        unk.cast::<IDispatch>()
            .map_err(|e| format!("Outlook IDispatch: {e}"))
    }
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
    ole_date_to_unix(&v)
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
        return Ok(0);
    }
    let ole = f64::try_from(v).map_err(|e| format!("expected date VARIANT: {e}"))?;
    let seconds = ((ole - 25569.0) * 86400.0).round() as i64;
    Ok(seconds)
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
