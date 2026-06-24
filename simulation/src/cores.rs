use core_affinity::CoreId;
use tracing::info;

pub struct CoreLayout {
    pub tokio: CoreId,
    pub runloop: CoreId,
    pub udp: CoreId,
    pub tcp: CoreId,
}

pub fn pick_cores() -> anyhow::Result<CoreLayout> {
    let cores = core_affinity::get_core_ids().unwrap_or_default();
    info!(
        count = cores.len(),
        ids = ?cores.iter().map(|c| c.id).collect::<Vec<_>>(),
        "available cores reported by core_affinity",
    );
    if cores.len() < 4 {
        anyhow::bail!(
            "simulation needs at least 4 cores for its pinned thread layout, found {}",
            cores.len()
        );
    }
    let cores = CoreLayout {
        tokio: cores[0],
        runloop: cores[1],
        udp: cores[2],
        tcp: cores[3],
    };
    info!(
        tokio = ?cores.tokio,
        runloop = ?cores.runloop,
        udp = ?cores.udp,
        tcp = ?cores.tcp,
        "pinned core layout",
    );
    Ok(cores)
}

pub fn pin_and_verify(expected: CoreId) {
    let ok = core_affinity::set_for_current(expected);
    if !ok {
        panic!("core_affinity::set_for_current returned false");
    }
}
