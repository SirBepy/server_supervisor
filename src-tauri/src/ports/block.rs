//! The persisted project-block allocator: `project_port`/`project_hub_port`
//! and everything that carves a project's stable `BLOCK_SIZE`-wide port
//! range out of `BASE..MAX` and persists it in `ports.json` (block markers,
//! per-owner slots, release). The ephemeral per-run acquisition path
//! (`acquire`/`release`/`mark_acquired`) lives in the sibling `acquire`
//! module instead - it never touches these block markers, only reads around
//! them (`port_in_any_block`).

use super::{save, PortEntry, PortRegistry, BASE, BLOCK_PREFIX, BLOCK_SIZE, HUB_OFFSET, MAX};

/// Owner label for a project's proxy-hub port reservation. Starts with
/// `{project_id}:`, so `release_project` (which strips everything prefixed
/// `{project_id}:`) reclaims it along with every command's slot.
fn hub_owner(project_id: &str) -> String {
    format!("{project_id}:__proxyhub__")
}

impl PortRegistry {
    /// Resolve (and persist) the stable port for one project command, reusing
    /// the same `reserved` pool `ports.json` already persists rather than a
    /// second store. `owner` is the command's stable composite id
    /// (`project:command` - see `types::unit_id`), which survives project and
    /// command renames since those only touch the display name.
    ///
    /// `override_port`: `Some(p)` pins the command to `p` - validated in-range
    /// and not already held by a *different* owner - and persists it verbatim
    /// (a manual override). `None` returns this owner's existing block-slot
    /// port if it already legitimately has one (stable across restarts and
    /// across re-clearing a since-removed override), else allocates the next
    /// free slot in the project's port block(s): a `BLOCK_SIZE`-wide range
    /// aligned to a multiple of `BLOCK_SIZE` inside `BASE..MAX`, extending to a
    /// fresh block once the current one(s) fill up (see `block_slot`).
    pub fn project_port(
        &self,
        project_id: &str,
        owner: &str,
        override_port: Option<u16>,
    ) -> Result<u16, String> {
        if let Some(p) = override_port {
            if !(BASE..MAX).contains(&p) {
                return Err(format!("port must be between {BASE} and {MAX}"));
            }
            // Reject a port already held by a different *real* owner (another
            // command, or another project's own manual override). Block
            // markers are internal bookkeeping, not a "real" owner, so they're
            // excluded here and reported via the block-range check below
            // instead - which gives a clearer message for that case.
            {
                let g = self.reserved.lock().unwrap();
                if let Some(clash) = g
                    .iter()
                    .find(|e| e.port == p && e.owner != owner && !e.owner.starts_with(BLOCK_PREFIX))
                {
                    return Err(format!("port {p} is already reserved by {}", clash.owner));
                }
            }
            // Reject a port inside a DIFFERENT project's allocated block, even
            // if no individual command in it has claimed that exact offset
            // yet - the whole block is that project's, not just its used slots.
            if let Some(other) = self.project_owning_block(p) {
                if other != project_id {
                    return Err(format!("port {p} is inside {other}'s port block"));
                }
            }
            self.set_owner_port(owner, p, "manual override");
            return Ok(p);
        }

        if let Some(p) = self.owner_port(owner) {
            if self.port_in_project_block(project_id, p) {
                return Ok(p);
            }
        }
        let p = self.block_slot(project_id)?;
        self.set_owner_port(owner, p, "project port block");
        Ok(p)
    }

    /// Resolve (and persist) the FIXED port for a project's reverse-proxy hub
    /// listener (see `supervisor::proxy_hub`): offset `HUB_OFFSET` (the top
    /// slot) of the project's FIRST allocated port block, i.e. `base+9`. A
    /// dev app bakes this address in at compile time, so it must never move -
    /// once assigned it is a real owner reservation (`project:__proxyhub__`,
    /// not a `BLOCK_PREFIX` marker), which also blocks a regular command from
    /// later claiming that same offset via `block_slot`.
    ///
    /// Idempotent: a project that already has a hub port keeps it. Ensures a
    /// block exists (allocating the project's first one if this is the very
    /// first port it has ever claimed). Errors if the offset is already held
    /// by a different real owner - e.g. a project with a full 10-command
    /// block that predates this feature; a rare edge case left as a clear
    /// error rather than silently reassigning someone else's port.
    pub fn project_hub_port(&self, project_id: &str) -> Result<u16, String> {
        let owner = hub_owner(project_id);
        if let Some(p) = self.owner_port(&owner) {
            return Ok(p);
        }
        let base = self.first_block_base(project_id)?;
        let port = base + HUB_OFFSET;
        {
            let g = self.reserved.lock().unwrap();
            if let Some(clash) = g
                .iter()
                .find(|e| e.port == port && e.owner != owner && !e.owner.starts_with(BLOCK_PREFIX))
            {
                return Err(format!(
                    "proxy hub port {port} is already claimed by {}",
                    clash.owner
                ));
            }
        }
        self.set_owner_port(&owner, port, "proxy hub");
        Ok(port)
    }

    /// The base port of `project_id`'s first allocated block, creating one
    /// (via `block_slot`, which on an empty project always lands offset 0 of
    /// a freshly allocated block - exactly the base) if it doesn't exist yet.
    fn first_block_base(&self, project_id: &str) -> Result<u16, String> {
        let prefix = block_owner_prefix(project_id);
        {
            let g = self.reserved.lock().unwrap();
            let mut bases: Vec<u16> = g
                .iter()
                .filter(|e| e.owner.starts_with(&prefix))
                .map(|e| e.port)
                .collect();
            bases.sort_unstable();
            if let Some(&b) = bases.first() {
                return Ok(b);
            }
        }
        self.block_slot(project_id)
    }

    /// Drop a single owner's port reservation (its project-block slot or
    /// manual override), freeing that port for reuse - called when its
    /// command is deleted.
    pub fn release_owner(&self, owner: &str) {
        let mut g = self.reserved.lock().unwrap();
        let before = g.len();
        g.retain(|e| e.owner != owner);
        if g.len() != before {
            save(&self.data_dir, &g);
        }
    }

    /// Release every port reservation belonging to a project - every
    /// command's slot plus the project's block marker(s) - so the whole block
    /// can be reclaimed by a future project. Called when a project is deleted.
    pub fn release_project(&self, project_id: &str) {
        let block_prefix = block_owner_prefix(project_id);
        let cmd_prefix = format!("{project_id}:");
        let mut g = self.reserved.lock().unwrap();
        let before = g.len();
        g.retain(|e| !(e.owner.starts_with(&block_prefix) || e.owner.starts_with(&cmd_prefix)));
        if g.len() != before {
            save(&self.data_dir, &g);
        }
    }

    /// The port currently reserved for `owner`, if any.
    fn owner_port(&self, owner: &str) -> Option<u16> {
        self.reserved
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.owner == owner)
            .map(|e| e.port)
    }

    /// True if `port` falls inside a port block already allocated to `project_id`.
    fn port_in_project_block(&self, project_id: &str, port: u16) -> bool {
        let prefix = block_owner_prefix(project_id);
        self.reserved
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.owner.starts_with(&prefix))
            .any(|e| (e.port..e.port + BLOCK_SIZE).contains(&port))
    }

    /// The id of the project whose block contains `port`, if any.
    fn project_owning_block(&self, port: u16) -> Option<String> {
        let g = self.reserved.lock().unwrap();
        g.iter().find_map(|e| {
            let rest = e.owner.strip_prefix(BLOCK_PREFIX)?;
            let (project_id, _block) = rest.rsplit_once(':')?;
            (e.port..e.port + BLOCK_SIZE)
                .contains(&port)
                .then(|| project_id.to_string())
        })
    }

    /// Ports literally held by a *real* owner - every reservation except the
    /// internal block markers - plus in-flight ephemeral acquisitions. Used to
    /// find a reusable offset within a project's own already-allocated
    /// block(s): the block's own marker must NOT count as "occupying" its
    /// base forever, or a released command's slot could never be reused (see
    /// `block_slot`).
    fn real_taken_set(&self) -> std::collections::HashSet<u16> {
        let mut set: std::collections::HashSet<u16> = self
            .reserved
            .lock()
            .unwrap()
            .iter()
            .filter(|e| !e.owner.starts_with(BLOCK_PREFIX))
            .map(|e| e.port)
            .collect();
        set.extend(self.acquired.lock().unwrap().iter().copied());
        set
    }

    /// Overwrite (or insert) the single reserved entry for `owner`, dropping
    /// any prior entry it held. Persists immediately.
    fn set_owner_port(&self, owner: &str, port: u16, note: &str) {
        let mut g = self.reserved.lock().unwrap();
        g.retain(|e| e.owner != owner);
        g.push(PortEntry { owner: owner.to_string(), port, note: note.to_string() });
        save(&self.data_dir, &g);
    }

    /// The next free slot for `project_id`: the first unoccupied offset in one
    /// of its already-allocated blocks, else the base of a freshly allocated
    /// block (persisted as a marker entry so future lookups find it again).
    /// Blocks are tried in the order they were created, so a project's ports
    /// stay compact rather than spreading out. `taken` combines every
    /// persisted reservation (this project's own slots, other projects'
    /// blocks/overrides, always-on-app reservations) with in-flight ephemeral
    /// acquisitions, so a slot squatted by something outside this bookkeeping
    /// is correctly skipped rather than silently double-assigned.
    fn block_slot(&self, project_id: &str) -> Result<u16, String> {
        let prefix = block_owner_prefix(project_id);
        let mut bases: Vec<u16> = {
            let g = self.reserved.lock().unwrap();
            g.iter()
                .filter(|e| e.owner.starts_with(&prefix))
                .map(|e| e.port)
                .collect()
        };
        bases.sort_unstable();

        // Real occupancy (excludes the blocks' own markers) - so a released
        // command's offset shows free again instead of being permanently
        // shadowed by the marker that carved out the block in the first place.
        let real_taken = self.real_taken_set();
        for base in &bases {
            for offset in 0..BLOCK_SIZE {
                let p = base + offset;
                if !real_taken.contains(&p) {
                    return Ok(p);
                }
            }
        }

        // Every existing block (if any) is full: allocate a fresh one, aligned
        // to BLOCK_SIZE, at the lowest range not already claimed by anyone -
        // this DOES need the marker-inclusive set, so we never re-pick a base
        // some other project's block already sits on.
        let taken = self.taken_set();
        debug_assert_eq!(BASE % BLOCK_SIZE, 0, "BASE must be block-aligned");
        let mut candidate = BASE;
        loop {
            if candidate.saturating_add(BLOCK_SIZE) > MAX {
                return Err(format!("no free port block available in {BASE}..{MAX}"));
            }
            if !taken.contains(&candidate) {
                break;
            }
            candidate += BLOCK_SIZE;
        }
        let owner = block_owner(project_id, bases.len() as u16);
        self.reserve(&owner, candidate, "project port block");
        Ok(candidate)
    }
}

/// Owner label for a project's Nth port block marker - a `PortEntry` whose
/// literal `port` is that block's base (see `PortRegistry::block_slot`).
/// Never collides with a real command's owner (`project:command`, no
/// `BLOCK_PREFIX` prefix).
fn block_owner(project_id: &str, block: u16) -> String {
    format!("{BLOCK_PREFIX}{project_id}:{block}")
}

fn block_owner_prefix(project_id: &str) -> String {
    format!("{BLOCK_PREFIX}{project_id}:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_port_allocates_an_aligned_block_in_stable_order() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let p0 = reg.project_port("proj", "proj:c0", None).unwrap();
        assert_eq!(p0, BASE, "first project claims the very first block");
        for i in 1..10u16 {
            let owner = format!("proj:c{i}");
            let p = reg.project_port("proj", &owner, None).unwrap();
            assert_eq!(p, p0 + i, "commands fill the block in stable (first-assigned) order");
        }
        // Re-asking for an already-assigned owner (e.g. a later start()) must
        // return the identical port, not a new one.
        assert_eq!(reg.project_port("proj", "proj:c0", None).unwrap(), p0);
    }

    #[test]
    fn project_port_exhausts_into_a_second_aligned_block() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let mut first_base = 0u16;
        for i in 0..10u16 {
            let p = reg.project_port("proj", &format!("proj:c{i}"), None).unwrap();
            if i == 0 {
                first_base = p;
            }
        }
        let overflow = reg.project_port("proj", "proj:c10", None).unwrap();
        assert_eq!(overflow % BLOCK_SIZE, 0, "the overflow block is itself block-aligned");
        for i in 0..10u16 {
            assert_ne!(overflow, first_base + i, "must not collide into its own first block");
        }
    }

    #[test]
    fn project_port_second_block_skips_a_neighbouring_projects_range() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        // A neighbour claims the very first block.
        let neighbor_port = reg.project_port("neighbor", "neighbor:web", None).unwrap();
        assert_eq!(neighbor_port, BASE);

        // "big" fills a full block of 10 - forced past the neighbour's range.
        let mut big_base = 0u16;
        for i in 0..10u16 {
            let p = reg.project_port("big", &format!("big:c{i}"), None).unwrap();
            if i == 0 {
                big_base = p;
            }
        }
        assert_ne!(big_base, neighbor_port, "big must not reuse the neighbour's slot");

        // big's 11th command overflows into a third block - neither the
        // neighbour's range nor its own first block.
        let overflow = reg.project_port("big", "big:c10", None).unwrap();
        assert_ne!(overflow, neighbor_port);
        for i in 0..10u16 {
            assert_ne!(overflow, big_base + i);
        }
    }

    #[test]
    fn project_port_override_rejects_collision_with_another_owner() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let taken = reg.project_port("proj-a", "proj-a:web", None).unwrap();

        let err = reg
            .project_port("proj-b", "proj-b:web", Some(taken))
            .unwrap_err();
        assert!(err.contains("proj-a:web"), "error names the conflicting owner: {err}");

        // The SAME owner re-affirming its own port is fine (idempotent), and a
        // manual override always wins over whatever slot it already held.
        assert_eq!(
            reg.project_port("proj-a", "proj-a:web", Some(taken)).unwrap(),
            taken
        );
    }

    #[test]
    fn project_port_override_rejects_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        assert!(reg.project_port("proj", "proj:web", Some(80)).is_err());
        assert!(reg.project_port("proj", "proj:web", Some(65000)).is_err());
    }

    #[test]
    fn project_port_persists_across_reload() {
        let dir = tempfile::tempdir().unwrap();
        let p0;
        {
            let reg = PortRegistry::new(dir.path().to_path_buf());
            p0 = reg.project_port("proj", "proj:web", None).unwrap();
            // A manual override on a second command must also survive reload.
            reg.project_port("proj", "proj:api", Some(48000)).unwrap();
        }
        let reg2 = PortRegistry::new(dir.path().to_path_buf());
        assert_eq!(
            reg2.project_port("proj", "proj:web", None).unwrap(),
            p0,
            "auto-assigned block slot survives a restart"
        );
        // A caller that still wants the override keeps re-supplying it (the
        // supervisor always re-reads it from the persisted Command); the
        // registry's job is just to keep honoring the same owner+port pairing.
        assert_eq!(
            reg2.project_port("proj", "proj:api", Some(48000)).unwrap(),
            48000,
            "manual override survives a restart when re-supplied"
        );
        // The raw reservation is genuinely on disk, not just re-derived.
        assert!(reg2.list().iter().any(|e| e.owner == "proj:api" && e.port == 48000));
    }

    #[test]
    fn release_owner_frees_its_slot_for_reuse() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let p0 = reg.project_port("proj", "proj:c0", None).unwrap();
        reg.release_owner("proj:c0");
        assert!(reg.list().iter().all(|e| e.owner != "proj:c0"));
        // A fresh command in the same project reclaims the freed slot rather
        // than being pushed into a new block.
        let p1 = reg.project_port("proj", "proj:c1", None).unwrap();
        assert_eq!(p1, p0, "freed slot is reused");
    }

    #[test]
    fn release_project_frees_the_whole_block() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let p0 = reg.project_port("proj", "proj:c0", None).unwrap();
        reg.release_project("proj");
        assert!(
            reg.list().iter().all(|e| !e.owner.starts_with("proj") && !e.owner.contains(":proj:")),
            "both the command slot and the block marker are gone"
        );
        // A different project can now claim the same freed range.
        let reclaimed = reg.project_port("other", "other:c0", None).unwrap();
        assert_eq!(reclaimed, p0);
    }

    #[test]
    fn project_hub_port_is_base_plus_nine_and_stable() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let hub = reg.project_hub_port("proj").unwrap();
        assert_eq!(hub, BASE + (BLOCK_SIZE - 1), "hub claims the block's top slot");
        // Idempotent: re-asking returns the identical port.
        assert_eq!(reg.project_hub_port("proj").unwrap(), hub);
        // A command allocated afterward must skip the offset the hub owns.
        let cmd_port = reg.project_port("proj", "proj:c0", None).unwrap();
        assert_ne!(cmd_port, hub, "a command must not collide with the hub's reserved offset");
    }

    #[test]
    fn project_hub_port_survives_reload_and_ten_full_commands_skip_it() {
        let dir = tempfile::tempdir().unwrap();
        let hub;
        {
            let reg = PortRegistry::new(dir.path().to_path_buf());
            hub = reg.project_hub_port("proj").unwrap();
            // Fill the rest of the block: 9 commands should fit in the 9
            // remaining offsets (0..8), the 10th must overflow to a new block
            // rather than colliding with the hub's offset 9.
            for i in 0..9u16 {
                let p = reg.project_port("proj", &format!("proj:c{i}"), None).unwrap();
                assert_ne!(p, hub);
            }
            let overflow = reg.project_port("proj", "proj:c9", None).unwrap();
            assert_ne!(overflow, hub);
            assert_eq!(overflow % BLOCK_SIZE, 0, "the 10th command overflows into a fresh aligned block");
        }
        let reg2 = PortRegistry::new(dir.path().to_path_buf());
        assert_eq!(reg2.project_hub_port("proj").unwrap(), hub, "hub port survives reload");
    }
}
