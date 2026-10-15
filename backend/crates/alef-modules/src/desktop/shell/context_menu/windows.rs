// SPDX-License-Identifier: MIT OR Apache-2.0
//! Windows Explorer context menus are kept entirely on their owning COM STA.
use super::ContextMenuItem;
use alef_core::{AlefError, ErrorCode};
use std::{
    collections::HashMap,
    ffi::c_void,
    mem::{size_of, zeroed},
    os::windows::ffi::OsStrExt,
    path::PathBuf,
    ptr,
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread,
};
use windows::{
    core::{Interface, PCWSTR, PWSTR},
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED},
        UI::{
            Shell::{
                IContextMenu, IContextMenu2, IContextMenu3, IShellFolder, SHBindToParent,
                SHParseDisplayName, CMF_EXPLORE, CMF_EXTENDEDVERBS, CMF_NORMAL,
                CMINVOKECOMMANDINFOEX, GCS_VERBW,
            },
            WindowsAndMessaging::{
                CreatePopupMenu, DestroyMenu, DispatchMessageW, GetMenuItemCount, GetMenuItemInfoW,
                PeekMessageW, TranslateMessage, MENUITEMINFOW, MENU_ITEM_MASK, MFS_CHECKED,
                MFS_DEFAULT, MFS_DISABLED, MFS_GRAYED, MFT_SEPARATOR, MIIM_FTYPE, MIIM_ID,
                MIIM_STATE, MIIM_STRING, MIIM_SUBMENU, MSG, PM_REMOVE, SW_SHOWNORMAL,
            },
        },
    },
};

const FIRST_ID: u32 = 1;
const LAST_ID: u32 = 0x7fff;
const MAX_DEPTH: usize = 16;
const MAX_ITEMS: usize = 2048;
const QUEUE_CAPACITY: usize = 8;
const STA_WAIT_MS: u32 = 25;

fn error(code: ErrorCode, message: &'static str) -> AlefError {
    AlefError::new(code, message)
}
fn internal() -> AlefError {
    error(
        ErrorCode::Internal,
        "shell.contextMenu: Windows shell operation failed",
    )
}

/// Cross-thread state contains only a bounded sender, never an apartment-bound COM object.
pub(super) struct StaMenu {
    tx: SyncSender<Command>,
}
enum Command {
    Invoke {
        offset: u32,
        paths: Vec<PathBuf>,
        runas_allowed: bool,
        reply: SyncSender<Result<(), AlefError>>,
    },
}
struct StaInit {
    rx: Receiver<Command>,
    ready: SyncSender<Result<Vec<ContextMenuItem>, AlefError>>,
    paths: Vec<PathBuf>,
    extended: bool,
}
impl StaMenu {
    pub(super) fn open(
        paths: &[PathBuf],
        extended: bool,
    ) -> Result<(StaMenu, Vec<ContextMenuItem>), AlefError> {
        if paths.is_empty() {
            return Err(error(
                ErrorCode::InvalidArgument,
                "context menu requires a path",
            ));
        }
        let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (ready, ready_rx) = mpsc::sync_channel(1);
        let init = StaInit {
            rx,
            ready,
            paths: paths.to_vec(),
            extended,
        };
        thread::Builder::new()
            .name("alef-context-menu-sta".into())
            .spawn(move || sta_main(init))
            .map_err(|_| internal())?;
        let items = ready_rx.recv().map_err(|_| internal())??;
        Ok((StaMenu { tx }, items))
    }
    pub(super) fn invoke(
        &self,
        offset: u32,
        paths: &[PathBuf],
        runas_allowed: bool,
    ) -> Result<(), AlefError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.tx
            .try_send(Command::Invoke {
                offset,
                paths: paths.to_vec(),
                runas_allowed,
                reply,
            })
            .map_err(|_| {
                error(
                    ErrorCode::Busy,
                    "shell.contextMenu: worker queue full or closed",
                )
            })?;
        // Shell extensions cannot be forcibly cancelled; this blocks only the caller's blocking-pool thread.
        result.recv().map_err(|_| internal())?
    }
}
fn sta_main(init: StaInit) {
    // SAFETY: this fresh thread performs no COM operation before initializing its STA.
    if unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_err() {
        let _ = init.ready.send(Err(internal()));
        return;
    }
    let cleanup = ComCleanup;
    match create_menu(&init.paths, init.extended) {
        Ok((context, menu, items, verbs)) => {
            let _ = init.ready.send(Ok(items));
            pump(init.rx, context, menu, verbs);
        }
        Err(err) => {
            let _ = init.ready.send(Err(err));
        }
    }
    drop(cleanup);
}
struct ComCleanup;
impl Drop for ComCleanup {
    fn drop(&mut self) {
        // SAFETY: paired once with successful CoInitializeEx on this same thread, after shell locals drop.
        unsafe { CoUninitialize() }
    }
}
struct Pidl(*mut windows::Win32::UI::Shell::Common::ITEMIDLIST);
impl Drop for Pidl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: CoTaskMemFree consumes the task-allocator PIDL once.
            unsafe { CoTaskMemFree(Some(self.0.cast::<c_void>())) }
        }
    }
}
type HMenu = windows::Win32::UI::WindowsAndMessaging::HMENU;
struct MenuHandle(HMenu);
impl Drop for MenuHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: this unique wrapper owns the popup allocated by CreatePopupMenu.
            let _ = unsafe { DestroyMenu(self.0) };
        }
    }
}

fn create_menu(
    paths: &[PathBuf],
    extended: bool,
) -> Result<
    (
        IContextMenu,
        MenuHandle,
        Vec<ContextMenuItem>,
        HashMap<u32, (String, bool)>,
    ),
    AlefError,
> {
    let mut full = Vec::with_capacity(paths.len());
    let mut folders: Vec<IShellFolder> = Vec::with_capacity(paths.len());
    let mut relatives: Vec<*const windows::Win32::UI::Shell::Common::ITEMIDLIST> =
        Vec::with_capacity(paths.len());
    for path in paths {
        let os = path.as_os_str();
        if os.is_empty() || os.encode_wide().any(|unit| unit == 0) {
            return Err(error(
                ErrorCode::InvalidArgument,
                "invalid context menu path",
            ));
        }
        let wide = os.encode_wide().chain(Some(0)).collect::<Vec<_>>();
        let mut raw = ptr::null_mut();
        // SAFETY: wide is NUL-terminated for the call; raw receives an owned absolute PIDL.
        unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut raw, 0, None) }
            .map_err(|_| internal())?;
        if raw.is_null() {
            return Err(internal());
        }
        full.push(Pidl(raw));
        let mut last = ptr::null_mut();
        // SAFETY: raw is a live full PIDL; SHBindToParent returns an interface and a borrowed child pointer.
        let folder: IShellFolder =
            unsafe { SHBindToParent(raw, Some(&mut last)) }.map_err(|_| internal())?;
        if last.is_null() {
            return Err(internal());
        }
        folders.push(folder);
        relatives.push(last.cast_const());
    }
    // SAFETY: the interface and borrowed PIDLs are valid for the synchronous call.
    let context: IContextMenu =
        unsafe { folders[0].GetUIObjectOf(HWND::default(), &relatives, None) }
            .map_err(|_| internal())?;
    let flags = CMF_NORMAL | CMF_EXPLORE | if extended { CMF_EXTENDEDVERBS } else { 0 };
    // SAFETY: CreatePopupMenu returns a fresh valid menu handle on this thread.
    let menu = MenuHandle(unsafe { CreatePopupMenu() }.map_err(|_| internal())?);
    // SAFETY: QueryContextMenu uses the live STA-owned menu with a bounded ID range.
    let hr = unsafe { context.QueryContextMenu(menu.0, 0, FIRST_ID, LAST_ID, flags) };
    if hr.is_err() {
        return Err(internal());
    }
    let mut verbs = HashMap::new();
    let mut reader = NativeReader {
        context: &context,
        count: 0,
    };
    let items = walk_reader(&mut reader, menu.0, 0, true, &mut verbs)?;
    Ok((context, menu, items, verbs))
}

fn command_verb(context: &IContextMenu, offset: u32) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: `GCS_VERBW` writes UTF-16 into the supplied writable buffer.
    let result = unsafe {
        context.GetCommandString(
            offset as usize,
            GCS_VERBW,
            None,
            windows::core::PSTR(buffer.as_mut_ptr().cast()),
            buffer.len() as u32,
        )
    };
    if result.is_ok() {
        String::from_utf16_lossy(
            &buffer[..buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len())],
        )
    } else {
        String::new()
    }
}

trait MenuReader {
    type Menu: Copy;
    fn entries(&mut self, menu: Self::Menu) -> Result<Vec<MenuEntry<Self::Menu>>, AlefError>;
}
struct MenuEntry<M> {
    id: u32,
    label: String,
    verb: String,
    enabled: bool,
    checked: bool,
    default: bool,
    separator: bool,
    submenu: Option<M>,
}
struct NativeReader<'a> {
    context: &'a IContextMenu,
    count: usize,
}
impl MenuReader for NativeReader<'_> {
    type Menu = HMenu;
    fn entries(&mut self, menu: HMenu) -> Result<Vec<MenuEntry<HMenu>>, AlefError> {
        // SAFETY: menu is a live handle in the current STA's owned menu tree.
        let count = unsafe { GetMenuItemCount(Some(menu)) };
        if count < 0 {
            return Err(internal());
        }
        let mut entries = Vec::with_capacity((count as usize).min(MAX_ITEMS));
        for position in 0..count as u32 {
            self.count = self.count.checked_add(1).ok_or_else(internal)?;
            if self.count > MAX_ITEMS {
                return Err(error(
                    ErrorCode::InvalidArgument,
                    "context menu item limit exceeded",
                ));
            }
            // SAFETY: zeroed C structure is valid output storage for MENUITEMINFOW.
            let mut info: MENUITEMINFOW = unsafe { zeroed() };
            info.cbSize = size_of::<MENUITEMINFOW>() as u32;
            info.fMask = MENU_ITEM_MASK(
                MIIM_FTYPE.0 | MIIM_ID.0 | MIIM_STATE.0 | MIIM_STRING.0 | MIIM_SUBMENU.0,
            );
            // SAFETY: menu and position are valid in this STA-owned menu tree; output storage is writable.
            unsafe { GetMenuItemInfoW(menu, position, true, &mut info) }.map_err(|_| internal())?;
            let mut text = vec![0u16; (info.cch as usize).saturating_add(1).clamp(1, 32768)];
            info.dwTypeData = PWSTR(text.as_mut_ptr());
            info.cch = u32::try_from(text.len()).map_err(|_| internal())?;
            // SAFETY: menu and position remain valid and text is writable for advertised cch.
            unsafe { GetMenuItemInfoW(menu, position, true, &mut info) }.map_err(|_| internal())?;
            let length = text.iter().position(|c| *c == 0).unwrap_or(text.len());
            let mut label = String::from_utf16_lossy(&text[..length]);
            let separator = info.fType.0 & MFT_SEPARATOR.0 != 0;
            let submenu = !info.hSubMenu.is_invalid();
            let verb = if !separator && !submenu && (FIRST_ID..=LAST_ID).contains(&info.wID) {
                command_verb(self.context, info.wID - FIRST_ID)
            } else {
                String::new()
            };
            if label.trim().is_empty() && !verb.is_empty() {
                label = verb.clone();
            }
            if submenu {
                let mut result = windows::Win32::Foundation::LRESULT::default();
                let message = 0x0117;
                let wparam = WPARAM(info.hSubMenu.0 as usize);
                let lparam = LPARAM(position as isize);
                if let Ok(ctx3) = self.context.cast::<IContextMenu3>() {
                    // SAFETY: WM_INITMENUPOPUP takes this valid owned submenu handle in wParam.
                    if unsafe { ctx3.HandleMenuMsg2(message, wparam, lparam, Some(&mut result)) }
                        .is_err()
                    {
                        if let Ok(ctx2) = self.context.cast::<IContextMenu2>() {
                            // SAFETY: same valid submenu and popup position, dispatched through fallback interface.
                            let _ = unsafe { ctx2.HandleMenuMsg(message, wparam, lparam) };
                        }
                    }
                } else if let Ok(ctx2) = self.context.cast::<IContextMenu2>() {
                    // SAFETY: WM_INITMENUPOPUP takes this valid owned submenu handle in wParam.
                    let _ = unsafe { ctx2.HandleMenuMsg(message, wparam, lparam) };
                }
            }
            entries.push(MenuEntry {
                id: info.wID,
                label,
                verb,
                enabled: info.fState.0 & (MFS_DISABLED.0 | MFS_GRAYED.0) == 0,
                checked: info.fState.0 & MFS_CHECKED.0 != 0,
                default: info.fState.0 & MFS_DEFAULT.0 != 0,
                separator,
                submenu: submenu.then_some(info.hSubMenu),
            });
        }
        Ok(entries)
    }
}
fn walk_reader<R: MenuReader>(
    reader: &mut R,
    root: R::Menu,
    depth: usize,
    parent_enabled: bool,
    verbs: &mut HashMap<u32, (String, bool)>,
) -> Result<Vec<ContextMenuItem>, AlefError> {
    fn visit<R: MenuReader>(
        reader: &mut R,
        menu: R::Menu,
        depth: usize,
        parent_enabled: bool,
        count: &mut usize,
        verbs: &mut HashMap<u32, (String, bool)>,
    ) -> Result<Vec<ContextMenuItem>, AlefError> {
        if depth >= MAX_DEPTH {
            return Err(error(
                ErrorCode::InvalidArgument,
                "context menu nesting limit exceeded",
            ));
        }
        let entries = reader.entries(menu)?;
        let mut items = Vec::with_capacity(entries.len().min(MAX_ITEMS));
        for entry in entries {
            *count = count.checked_add(1).ok_or_else(internal)?;
            if *count > MAX_ITEMS {
                return Err(error(
                    ErrorCode::InvalidArgument,
                    "context menu item limit exceeded",
                ));
            }
            let enabled = parent_enabled && entry.enabled;
            let submenu = match entry.submenu {
                Some(child) => Some(visit(reader, child, depth + 1, enabled, count, verbs)?),
                None => None,
            };
            let actionable =
                !entry.separator && submenu.is_none() && (FIRST_ID..=LAST_ID).contains(&entry.id);
            let id = if actionable {
                let offset = entry.id - FIRST_ID;
                if verbs.insert(offset, (entry.verb, enabled)).is_some() {
                    return Err(error(
                        ErrorCode::InvalidArgument,
                        "duplicate Windows command ID",
                    ));
                }
                format!("cmd:{offset}")
            } else {
                String::new()
            };
            items.push(ContextMenuItem {
                id,
                label: entry.label,
                enabled,
                checked: Some(entry.checked),
                default: Some(entry.default),
                separator: entry.separator.then_some(true),
                submenu,
                icon: None,
            });
        }
        Ok(items)
    }
    let mut count = 0usize;
    visit(reader, root, depth, parent_enabled, &mut count, verbs)
}

fn pump(
    rx: Receiver<Command>,
    context: IContextMenu,
    menu: MenuHandle,
    verbs: HashMap<u32, (String, bool)>,
) {
    loop {
        match rx.try_recv() {
            Ok(Command::Invoke {
                offset,
                paths,
                runas_allowed,
                reply,
            }) => {
                let result = invoke_sta(&context, menu.0, &verbs, offset, &paths, runas_allowed);
                // Receiver disappearance means the caller no longer needs this result.
                let _ = reply.try_send(result);
            }
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        // Bounded polling makes sender disconnect observable while servicing queued STA messages.
        // SAFETY: no object handles are supplied; wait polls this STA thread's input queue.
        let _ = unsafe {
            windows::Win32::UI::WindowsAndMessaging::MsgWaitForMultipleObjectsEx(
                None,
                STA_WAIT_MS,
                windows::Win32::UI::WindowsAndMessaging::QUEUE_STATUS_FLAGS(0x04ff),
                windows::Win32::UI::WindowsAndMessaging::MWMO_INPUTAVAILABLE,
            )
        };
        let mut message = MSG::default();
        // SAFETY: null HWND drains this thread's message queue into writable MSG storage.
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: the message is written by PeekMessageW and dispatched on the same STA.
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}
fn invoke_sta(
    context: &IContextMenu,
    menu: HMenu,
    verbs: &HashMap<u32, (String, bool)>,
    offset: u32,
    paths: &[PathBuf],
    runas_allowed: bool,
) -> Result<(), AlefError> {
    if offset > LAST_ID - FIRST_ID {
        return Err(error(
            ErrorCode::InvalidArgument,
            "invalid Windows command offset",
        ));
    }
    let Some((verb, enabled)) = verbs.get(&offset) else {
        return Err(error(
            ErrorCode::PermissionDenied,
            "context menu command unavailable",
        ));
    };
    if !enabled {
        return Err(error(
            ErrorCode::PermissionDenied,
            "context menu command disabled",
        ));
    }
    super::rules::verb_refused(verb, paths, runas_allowed)?;
    let _menu_lifetime = menu;
    let info = CMINVOKECOMMANDINFOEX {
        cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        fMask: 0x0000_4000, // CMIC_MASK_UNICODE
        hwnd: HWND::default(),
        lpVerb: windows::core::PCSTR(offset as usize as *const u8),
        lpVerbW: PCWSTR(offset as usize as *const u16),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: the structure is C-compatible, cbSize includes the extension, and its offset pointers are MAKEINTRESOURCE.
    let base = (&info as *const CMINVOKECOMMANDINFOEX)
        .cast::<windows::Win32::UI::Shell::CMINVOKECOMMANDINFO>();
    // SAFETY: the live COM interface accepts an initialized extended invoke structure on its STA.
    unsafe { context.InvokeCommand(base) }.map_err(|_| internal())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;
    struct Fake {
        trees: Map<u8, Vec<MenuEntry<u8>>>,
    }
    impl MenuReader for Fake {
        type Menu = u8;
        fn entries(&mut self, id: u8) -> Result<Vec<MenuEntry<u8>>, AlefError> {
            self.trees.remove(&id).ok_or_else(internal)
        }
    }
    fn leaf(id: u32, verb: &str, enabled: bool) -> MenuEntry<u8> {
        MenuEntry {
            id,
            label: format!("item-{id}"),
            verb: verb.into(),
            enabled,
            checked: false,
            default: false,
            separator: false,
            submenu: None,
        }
    }
    #[test]
    fn offsets_verbs_submenus_and_disabled_ancestors_are_preserved() {
        let mut fake = Fake {
            trees: Map::from([
                (
                    0,
                    vec![MenuEntry {
                        id: 3,
                        label: "Parent".into(),
                        verb: String::new(),
                        enabled: false,
                        checked: false,
                        default: false,
                        separator: false,
                        submenu: Some(1),
                    }],
                ),
                (1, vec![leaf(9, "runas", true)]),
            ]),
        };
        let mut verbs = HashMap::new();
        let items = walk_reader(&mut fake, 0, 0, true, &mut verbs).unwrap();
        assert_eq!(items[0].submenu.as_ref().unwrap()[0].id, "cmd:8");
        assert_eq!(verbs.get(&8), Some(&(String::from("runas"), false)));
    }
    #[test]
    fn separators_checked_and_default_items_keep_their_metadata() {
        let mut fake = Fake {
            trees: Map::from([(
                0,
                vec![MenuEntry {
                    id: 0,
                    label: String::new(),
                    verb: String::new(),
                    enabled: true,
                    checked: true,
                    default: true,
                    separator: true,
                    submenu: None,
                }],
            )]),
        };
        let items = walk_reader(&mut fake, 0, 0, true, &mut HashMap::new()).unwrap();
        assert_eq!(items[0].separator, Some(true));
        assert_eq!(items[0].checked, Some(true));
        assert_eq!(items[0].default, Some(true));
    }
    #[test]
    fn depth_and_item_limits_are_enforced() {
        let mut deep = Fake { trees: Map::new() };
        for id in 0..=MAX_DEPTH as u8 {
            deep.trees.insert(
                id,
                vec![MenuEntry {
                    id: 0,
                    label: String::new(),
                    verb: String::new(),
                    enabled: true,
                    checked: false,
                    default: false,
                    separator: false,
                    submenu: (id < MAX_DEPTH as u8).then_some(id + 1),
                }],
            );
        }
        let error = walk_reader(&mut deep, 0, 0, true, &mut HashMap::new()).unwrap_err();
        assert_eq!(error.message, "context menu nesting limit exceeded");
        let mut large = Fake {
            trees: Map::from([(
                0,
                (0..=MAX_ITEMS)
                    .map(|index| leaf(FIRST_ID + u32::try_from(index).unwrap(), "", true))
                    .collect(),
            )]),
        };
        let error = walk_reader(&mut large, 0, 0, true, &mut HashMap::new()).unwrap_err();
        assert_eq!(error.message, "context menu item limit exceeded");
    }
    #[test]
    fn duplicate_command_ids_are_rejected() {
        let mut duplicate = Fake {
            trees: Map::from([(0, vec![leaf(FIRST_ID, "", true), leaf(FIRST_ID, "", true)])]),
        };
        let error = walk_reader(&mut duplicate, 0, 0, true, &mut HashMap::new()).unwrap_err();
        assert_eq!(error.message, "duplicate Windows command ID");
    }
    #[test]
    fn refusal_rules_reject_runas_case_insensitively_and_batch_open() {
        let paths = vec![
            PathBuf::from("C:/tmp/a.txt"),
            PathBuf::from("C:/tmp/unsafe.BAT"),
        ];
        assert!(super::super::rules::verb_refused("RuNaS", &paths, false).is_err());
        assert!(super::super::rules::verb_refused("OPEN", &paths, true).is_err());
        assert!(super::super::rules::verb_refused("custom", &paths, false).is_ok());
    }
}
