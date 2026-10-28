use odysseia_automation::pulse::PulseTable;
use odysseia_automation::{AutomationGuardConfig, ClockAction, RedstoneClockGuard};
use odysseia_core::{BossType, ChatFilterEngine, ChatFilterRule, StaffTrollType, Vector3D};
use odysseia_telemetry::ServerStatusReport;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, OnceLock};

/// ABI version of the exported symbols. Bump on any signature/semantic change;
/// the Java side refuses to bind when it differs from the version it was built for.
pub const ODYSSEIA_ABI_VERSION: u32 = 1;

/// Runs `f` and never lets a panic cross the FFI boundary (that would be UB/abort the JVM).
#[inline]
fn guard<T>(default: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(default)
}

/// CString that never fails: interior NULs are dropped.
fn to_c(s: String) -> *mut c_char {
    let bytes: Vec<u8> = s.into_bytes().into_iter().filter(|b| *b != 0).collect();
    CString::new(bytes).map(CString::into_raw).unwrap_or(std::ptr::null_mut())
}

static PULSES: Mutex<Option<PulseTable>> = Mutex::new(None);

fn pulses() -> std::sync::MutexGuard<'static, Option<PulseTable>> {
    PULSES.lock().unwrap_or_else(|e| e.into_inner())
}

/// ABI version (see `ODYSSEIA_ABI_VERSION`).
#[no_mangle]
pub extern "C" fn odysseia_abi_version() -> u32 {
    ODYSSEIA_ABI_VERSION
}

/// (Re)creates the pulse table. `ring_cap` = timestamps kept per block (>= long pulse limit),
/// `max_entries` = max tracked blocks. Returns 0 on success.
#[no_mangle]
pub extern "C" fn odysseia_pulse_init(ring_cap: u32, max_entries: u32) -> i32 {
    guard(-1, || {
        *pulses() = Some(PulseTable::new(ring_cap as usize, max_entries as usize));
        0
    })
}

/// Records one rising-edge pulse and returns `(fast << 32) | long` (both u32 counts),
/// or -1 when uninitialised/full/failed (caller must fail open).
#[no_mangle]
pub extern "C" fn odysseia_pulse_record(
    world: i32, x: i32, y: i32, z: i32,
    now_ms: i64, long_window_ms: i64, fast_window_ms: i64,
) -> i64 {
    guard(-1, || {
        let mut g = pulses();
        match g.as_mut().and_then(|t| t.record([world, x, y, z], now_ms, long_window_ms, fast_window_ms)) {
            Some(c) => (((c.fast as u64) << 32) | c.long as u64) as i64,
            None => -1,
        }
    })
}

/// Forgets one block (after it was broken/throttled). Returns 0 on success.
#[no_mangle]
pub extern "C" fn odysseia_pulse_remove(world: i32, x: i32, y: i32, z: i32) -> i32 {
    guard(-1, || {
        if let Some(t) = pulses().as_mut() { t.remove(&[world, x, y, z]); }
        0
    })
}

/// Drops blocks idle for more than `idle_ms`. Returns how many were dropped (-1 on error).
#[no_mangle]
pub extern "C" fn odysseia_pulse_sweep(now_ms: i64, idle_ms: i64) -> i32 {
    guard(-1, || pulses().as_mut().map_or(0, |t| t.sweep(now_ms, idle_ms) as i32))
}

/// Clears every tracked block. Returns 0 on success.
#[no_mangle]
pub extern "C" fn odysseia_pulse_clear() -> i32 {
    guard(-1, || { if let Some(t) = pulses().as_mut() { t.clear(); } 0 })
}

/// Number of tracked blocks (-1 if uninitialised).
#[no_mangle]
pub extern "C" fn odysseia_pulse_len() -> i32 {
    guard(-1, || pulses().as_ref().map_or(-1, |t| t.len() as i32))
}

/// Batch aura test: one call per tick for all players. `points` = `3*n` f64 (x,y,z...),
/// `out` = `n` bytes (1 inside / 0 outside). Returns hits, or -1 on invalid args.
///
/// # Safety
/// `points` must be valid for `3*n` reads and `out` for `n` writes.
#[no_mangle]
pub unsafe extern "C" fn odysseia_aura_batch(
    bx: f64, by: f64, bz: f64, radius: f64,
    n: u32, points: *const f64, out: *mut u8,
) -> i32 {
    if n == 0 { return 0; }
    if points.is_null() || out.is_null() || n > (1 << 20) { return -1; }
    guard(-1, || {
        let pts = std::slice::from_raw_parts(points, 3 * n as usize);
        let out = std::slice::from_raw_parts_mut(out, n as usize);
        odysseia_core::aura_hits(Vector3D::new(bx, by, bz), radius, pts, out) as i32
    })
}

/// Resets the legacy per-block tick counters of `odysseia_evaluate_clock`.
#[no_mangle]
pub extern "C" fn odysseia_clock_reset_window() {
    guard((), || get_redstone_guard().reset_window());
}

static REDSTONE_GUARD: OnceLock<RedstoneClockGuard> = OnceLock::new();
static CHAT_FILTER: OnceLock<ChatFilterEngine> = OnceLock::new();

fn get_redstone_guard() -> &'static RedstoneClockGuard {
    REDSTONE_GUARD.get_or_init(|| RedstoneClockGuard::new(AutomationGuardConfig::default()))
}

fn get_chat_filter() -> &'static ChatFilterEngine {
    CHAT_FILTER.get_or_init(|| ChatFilterEngine::new(ChatFilterRule::default()))
}

/// Devuelve si un Boss es elegible para apariciones naturales en el mundo.
#[no_mangle]
pub extern "C" fn odysseia_is_boss_natural(boss_id_ptr: *const c_char) -> i32 {
    if boss_id_ptr.is_null() {
        return 0;
    }
    let c_str = unsafe { CStr::from_ptr(boss_id_ptr) };
    guard(0, || match c_str.to_str().ok().and_then(BossType::from_id) {
        Some(boss) if boss.is_natural_spawn_allowed() => 1,
        _ => 0,
    })
}

/// Verifica si la posición del jugador está dentro del aura de daño de un Boss.
#[no_mangle]
pub extern "C" fn odysseia_check_boss_aura(
    bx: f64, by: f64, bz: f64,
    px: f64, py: f64, pz: f64,
    aura_radius: f64
) -> i32 {
    let boss = Vector3D::new(bx, by, bz);
    let player = Vector3D::new(px, py, pz);
    i32::from(odysseia_core::is_in_boss_aura(boss, player, aura_radius))
}

/// Evalúa el comportamiento de un reloj de redstone con la política de escalación de fix/automation-guard:
/// Retorna: 0 (ALLOW), 1 (THROTTLE), 2 (BREAK).
#[no_mangle]
pub extern "C" fn odysseia_evaluate_clock(
    x: i32,
    y: i32,
    z: i32,
    is_clock_structure: i32,
    is_protected_slimefun: i32,
) -> i32 {
    guard(0, || match get_redstone_guard().evaluate_clock(x, y, z, is_clock_structure != 0, is_protected_slimefun != 0) {
        ClockAction::Allow => 0,
        ClockAction::Throttle => 1,
        ClockAction::Break => 2,
    })
}

/// Inspecciona un mensaje de chat para detectar palabras prohibidas.
#[no_mangle]
pub extern "C" fn odysseia_chat_filter_inspect(msg_ptr: *const c_char) -> *mut c_char {
    if msg_ptr.is_null() {
        return std::ptr::null_mut();
    }
    let c_str = unsafe { CStr::from_ptr(msg_ptr) };
    guard(std::ptr::null_mut(), || match c_str.to_str() {
        Ok(msg) => get_chat_filter().inspect_message(msg).map_or(std::ptr::null_mut(), to_c),
        Err(_) => std::ptr::null_mut(),
    })
}

/// Ejecuta un troll de Staff y devuelve el mensaje de feedback formateado en C-String.
#[no_mangle]
pub extern "C" fn odysseia_execute_troll(
    troll_type_id: i32,
    target_ptr: *const c_char
) -> *mut c_char {
    if target_ptr.is_null() {
        return std::ptr::null_mut();
    }
    let c_str = unsafe { CStr::from_ptr(target_ptr) };
    let target = c_str.to_str().unwrap_or("Jugador");

    let troll = match troll_type_id {
        0 => StaffTrollType::FakeOp,
        1 => StaffTrollType::FakeCrash,
        2 => StaffTrollType::VoidFall,
        3 => StaffTrollType::Anvil,
        4 => StaffTrollType::Creeper,
        5 => StaffTrollType::Spiders,
        6 => StaffTrollType::Lightning,
        _ => StaffTrollType::Screamer,
    };

    guard(std::ptr::null_mut(), || to_c(troll.get_feedback_message(target)))
}

/// Formatea la respuesta de estado para DiscordSRV en C-String.
#[no_mangle]
pub extern "C" fn odysseia_format_tps_message(
    tps1m: f64, tps5m: f64, tps15m: f64,
    used_ram: u64, max_ram: u64,
    _online_count: u32, max_players: u32
) -> *mut c_char {
    let report = ServerStatusReport {
        tps_1m: tps1m,
        tps_5m: tps5m,
        tps_15m: tps15m,
        used_ram_mb: used_ram,
        max_ram_mb: max_ram,
        online_players: vec![],
        max_players,
    };
    guard(std::ptr::null_mut(), || to_c(report.build_discord_status_message()))
}

/// Libera la memoria de una cadena C devuelta por Rust.
#[no_mangle]
pub extern "C" fn odysseia_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            let _ = CString::from_raw(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_and_pulse_roundtrip() {
        assert_eq!(odysseia_abi_version(), 1);
        assert_eq!(odysseia_pulse_record(0, 1, 2, 3, 0, 1000, 100), -1); // before init: fail open
        assert_eq!(odysseia_pulse_init(64, 128), 0);
        let mut r = 0;
        for i in 0..5 { r = odysseia_pulse_record(7, 1, 2, 3, 1000 + i, 120_000, 10_000); }
        assert_eq!((r >> 32) as u32, 5);
        assert_eq!((r & 0xffff_ffff) as u32, 5);
        assert_eq!(odysseia_pulse_len(), 1);
        assert_eq!(odysseia_pulse_sweep(10_000_000, 1000), 1);
        assert_eq!(odysseia_pulse_len(), 0);
    }

    #[test]
    fn aura_batch_and_null_safety() {
        let pts = [1.0, 0.0, 0.0, 9.0, 0.0, 0.0];
        let mut out = [0u8; 2];
        let hits = unsafe { odysseia_aura_batch(0.0, 0.0, 0.0, 5.0, 2, pts.as_ptr(), out.as_mut_ptr()) };
        assert_eq!((hits, out), (1, [1, 0]));
        assert_eq!(unsafe { odysseia_aura_batch(0.0, 0.0, 0.0, 5.0, 2, std::ptr::null(), out.as_mut_ptr()) }, -1);
        assert!(odysseia_chat_filter_inspect(std::ptr::null()).is_null());
        let s = odysseia_chat_filter_inspect(c"vendo DUPE barato".as_ptr());
        assert!(!s.is_null());
        odysseia_free_string(s);
    }
}
