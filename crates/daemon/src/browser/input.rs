// Operator input forwarded from the live viewport UI to CDP.
//
// Three threads of input:
//   - Mouse (`Input.dispatchMouseEvent`)
//   - Key  (`Input.dispatchKeyEvent`)
//   - IME composition (`Input.imeSetComposition` + `Input.insertText`)
//
// All coordinates are CSS pixels in the page's viewport; the UI sends them
// in the same frame the screencast is rendered in, so no scaling is done
// here (the UI's <img> is naturally CSS-pixel-sized via its width/height
// attributes, matching what `Page.startScreencast` returns).
//
// Note on access control: per the security model, the *human operator*
// driving the live viewport is trusted; element-class subtree restrictions
// apply only to the agent path (snapshot + @eN refs). The URL allowlist
// still binds — a mouse click that triggers a navigation to a disallowed
// URL is blocked by the Fetch interceptor.

use anyhow::Result;
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    ImeSetCompositionParams, InsertTextParams, MouseButton,
};
use chromiumoxide::Page;

#[derive(Debug, Clone, Copy)]
pub enum MouseKind {
    Pressed,
    Released,
    Moved,
    Wheel,
}

impl From<MouseKind> for DispatchMouseEventType {
    fn from(k: MouseKind) -> Self {
        match k {
            MouseKind::Pressed => DispatchMouseEventType::MousePressed,
            MouseKind::Released => DispatchMouseEventType::MouseReleased,
            MouseKind::Moved => DispatchMouseEventType::MouseMoved,
            MouseKind::Wheel => DispatchMouseEventType::MouseWheel,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Button {
    Left,
    Middle,
    Right,
    None,
}

impl From<Button> for MouseButton {
    fn from(b: Button) -> Self {
        match b {
            Button::Left => MouseButton::Left,
            Button::Middle => MouseButton::Middle,
            Button::Right => MouseButton::Right,
            Button::None => MouseButton::None,
        }
    }
}

pub async fn dispatch_mouse(
    page: &Page,
    kind: MouseKind,
    x: f64,
    y: f64,
    button: Button,
    click_count: i64,
    modifiers: i64,
) -> Result<()> {
    let buttons_field = match (kind, button) {
        (MouseKind::Pressed, Button::Left) => Some(1),
        (MouseKind::Pressed, Button::Right) => Some(2),
        (MouseKind::Pressed, Button::Middle) => Some(4),
        _ => Some(0),
    };
    page.execute(DispatchMouseEventParams {
        r#type: kind.into(),
        x,
        y,
        modifiers: Some(modifiers),
        timestamp: None,
        button: Some(button.into()),
        buttons: buttons_field,
        click_count: Some(click_count),
        force: None,
        tangential_pressure: None,
        tilt_x: None,
        tilt_y: None,
        twist: None,
        delta_x: None,
        delta_y: None,
        pointer_type: None,
    })
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub enum KeyKind {
    Down,
    Up,
    /// `char` — emits a text-insertion-only event (for printable keys
    /// where we don't have a real keystroke).
    Char,
    /// `rawKeyDown` — keydown without producing a `keypress` (good for
    /// arrow keys, F-keys, modifier-only combos).
    RawDown,
}

impl From<KeyKind> for DispatchKeyEventType {
    fn from(k: KeyKind) -> Self {
        match k {
            KeyKind::Down => DispatchKeyEventType::KeyDown,
            KeyKind::Up => DispatchKeyEventType::KeyUp,
            KeyKind::Char => DispatchKeyEventType::Char,
            KeyKind::RawDown => DispatchKeyEventType::RawKeyDown,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct KeyEvent {
    pub kind_down: bool,
    pub key: String,
    pub code: String,
    pub text: Option<String>,
    pub modifiers: i64,
    pub windows_virtual_key_code: Option<i64>,
}

pub async fn dispatch_key(page: &Page, ev: KeyEvent) -> Result<()> {
    // Send keyDown (or rawKeyDown) followed by keyUp so the page sees a
    // complete event pair. For printable keys we set `text`, which triggers
    // a `keypress` and the actual character insertion.
    let ty = if ev.kind_down {
        if ev.text.is_some() {
            DispatchKeyEventType::KeyDown
        } else {
            DispatchKeyEventType::RawKeyDown
        }
    } else {
        DispatchKeyEventType::KeyUp
    };
    page.execute(DispatchKeyEventParams {
        r#type: ty,
        modifiers: Some(ev.modifiers),
        timestamp: None,
        text: ev.text.clone(),
        unmodified_text: ev.text,
        key_identifier: None,
        code: Some(ev.code),
        key: Some(ev.key),
        windows_virtual_key_code: ev.windows_virtual_key_code,
        native_virtual_key_code: None,
        auto_repeat: None,
        is_keypad: None,
        is_system_key: None,
        location: None,
        commands: None,
    })
    .await?;
    Ok(())
}

/// IME composition update. The caller drives composition in three stages:
///   1. `compositionstart` → first call (text = "")
///   2. `compositionupdate` → `ime_set_composition(text, ..)` repeatedly
///   3. `compositionend`   → `insert_text(text)` (commits and clears IME)
pub async fn ime_set_composition(
    page: &Page,
    text: &str,
    selection_start: i64,
    selection_end: i64,
) -> Result<()> {
    page.execute(ImeSetCompositionParams {
        text: text.to_string(),
        selection_start,
        selection_end,
        replacement_start: None,
        replacement_end: None,
    })
    .await?;
    Ok(())
}

pub async fn insert_text(page: &Page, text: &str) -> Result<()> {
    page.execute(InsertTextParams {
        text: text.to_string(),
    })
    .await?;
    Ok(())
}
