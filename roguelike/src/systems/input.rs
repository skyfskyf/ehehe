use bevy::{ecs::system::SystemParam, prelude::*};
#[cfg(not(feature = "windowed"))]
use bevy_ratatui::event::KeyMessage;
#[cfg(not(feature = "windowed"))]
use ratatui::crossterm::event::KeyCode;

use crate::components::{Dead, Faction, Health, Inventory, ItemKind, SPELL_STAMINA_COST, Stamina, PlayerControlled, Position, Viewshed};
use crate::events::{MeleeWideIntent, MolotovCastIntent, MoveIntent, RangedAttackIntent, SpellCastIntent, ThrowItemIntent, UseItemIntent};
use crate::grid_vec::GridVec;
use crate::resources::{CombatLog, CursorPosition, DynamicRng, ExtraWorldTicks, GameState, InputMode, InputState, MapSeed, RestartRequested, SpectatingAfterDeath, TurnState};

/// Bundles all intent MessageWriters to stay under Bevy's 16-param system limit.
#[derive(SystemParam)]
pub struct IntentWriters<'w> {
    move_intents: MessageWriter<'w, MoveIntent>,
    spell_intents: MessageWriter<'w, SpellCastIntent>,
    molotov_intents: MessageWriter<'w, MolotovCastIntent>,
    use_item_intents: MessageWriter<'w, UseItemIntent>,
    ranged_intents: MessageWriter<'w, RangedAttackIntent>,
    melee_wide_intents: MessageWriter<'w, MeleeWideIntent>,
    throw_item_intents: MessageWriter<'w, ThrowItemIntent>,
}

/// Default radius for the player's grenade blast.
const SPELL_RADIUS: i32 = 3;

/// Range for the targeted ranged attack (bullet max travel distance).
const RANGED_ATTACK_RANGE: i32 = 100;

/// Maximum inventory slots for the player.
pub const MAX_INVENTORY_SIZE: usize = 6;

/// Stamina cost for the Throw Sand ability (G key).
const SAND_STAMINA_COST: i32 = 5;

/// Stamina cost for the Throw Item ability (E key).
const THROW_ITEM_STAMINA_COST: i32 = 10;

/// Stamina cost for the Roundhouse ability (F key).
const ROUNDHOUSE_STAMINA_COST: i32 = 10;

/// Stamina cost per tile of movement (WASD / arrows).
const MOVE_STAMINA_COST: i32 = 2;

/// A single command binding entry: the key(s) that trigger it, a short name, documentation,
/// and a category for UI grouping.
pub struct CommandBinding {
    /// Key combination string shown in the help/welcome screen.
    pub key: &'static str,
    /// Short action name.
    pub name: &'static str,
    /// Longer description / documentation for the command.
    pub docs: &'static str,
    /// Category for grouping in the Q menu: "Movement", "Combat", "Inventory", or "Other".
    pub category: &'static str,
}

/// All keybindings, generated from the exhaustive match arms below.
/// Used by the `?` help overlay to display available commands.
/// Related keys are grouped (WASD, IJKL) to reduce visual clutter.
pub const KEYBINDINGS: &[CommandBinding] = &[
    CommandBinding { key: "WASD / ↑↓←→", name: "Move (3 ticks)", docs: "Move the player one tile. Physical movement costs 3 ticks and 2 stamina.", category: "Movement" },
    CommandBinding { key: "IJKL", name: "Cursor (1 tick)", docs: "Move the cursor one tile for aiming. Costs 1 tick.", category: "Other" },
    CommandBinding { key: "C", name: "Center cursor", docs: "Snap cursor onto your position. Costs 1 tick.", category: "Other" },
    CommandBinding { key: "V", name: "Auto-aim", docs: "Cursor steps toward nearest enemy. Costs 1 tick.", category: "Other" },
    CommandBinding { key: "R", name: "Reload (6 ticks)", docs: "Reload gun (1 bullet + 1 cap + 1 powder). Costs 6 ticks.", category: "Combat" },
    CommandBinding { key: "F", name: "Roundhouse (2 ticks)", docs: "Roundhouse kick all adjacent enemies. Costs 2 ticks + 10 stamina.", category: "Combat" },
    CommandBinding { key: "T", name: "Wait (1 tick)", docs: "Skip your turn. Costs 1 tick.", category: "Movement" },
    CommandBinding { key: "G", name: "Throw sand (5 sta)", docs: "Create sand cloud blocking vision toward cursor.", category: "Combat" },
    CommandBinding { key: "E", name: "Throw item (10 sta)", docs: "Throw a random inventory item toward cursor.", category: "Inventory" },
    CommandBinding { key: "1-0", name: "Fire/Use (2 ticks)", docs: "Use item by slot. Guns/grenades fire toward cursor. Costs 2 ticks.", category: "Inventory" },
    CommandBinding { key: "Q", name: "Menu", docs: "Toggle pause menu", category: "Other" },
];

/// Reads keyboard input (terminal/crossterm mode).
/// Global keys (quit, pause, help) are always handled.
/// Movement keys are only processed while `TurnState::AwaitingInput`,
/// which transitions the game into `PlayerTurn` so that the action is
/// resolved before the next input is accepted.
///
/// When the game is in `GameState::Dead`, only quit (Q) and restart (R) work.
#[cfg(not(feature = "windowed"))]
pub fn input_system(
    mut messages: MessageReader<KeyMessage>,
    mut intents: IntentWriters,
    player_query: Query<(Entity, &Position, Option<&Stamina>, Option<&Inventory>, Option<&Dead>), With<PlayerControlled>>,
    mut player_viewshed: Query<&mut Viewshed, With<PlayerControlled>>,
    item_kind_query: Query<&ItemKind>,
    (hostiles_query, _health_query): (Query<&Position, (With<Faction>, Without<PlayerControlled>)>, Query<Entity, With<Health>>),
    game_state: Res<State<GameState>>,
    mut next_game_state: ResMut<NextState<GameState>>,
    turn_state: Option<Res<State<TurnState>>>,
    mut next_turn_state: Option<ResMut<NextState<TurnState>>>,
    mut combat_log: ResMut<CombatLog>,
    mut input_state: ResMut<InputState>,
    mut restart_requested: ResMut<RestartRequested>,
    mut cursor: ResMut<CursorPosition>,
    (mut extra_world_ticks, spectating, dynamic_rng, seed, _spatial): (ResMut<ExtraWorldTicks>, ResMut<SpectatingAfterDeath>, Res<DynamicRng>, Res<MapSeed>, Res<crate::resources::SpatialIndex>),
    mut god_mode: ResMut<crate::resources::GodMode>,
) {
    // Handle Dead and Victory states: R to restart, auto-advance turns when dead.
    if *game_state.get() == GameState::Dead || *game_state.get() == GameState::Victory {
        for message in messages.read() {
            if let KeyCode::Char('r') = message.code {
                restart_requested.0 = true;
            }
        }
        return;
    }

    let Ok((player_entity, player_pos, player_stamina, player_inv, player_dead)) = player_query.single() else {
        // PlayerControlled entity is gone (should only happen transiently).
        messages.read().for_each(|_| {});
        return;
    };

    // If the player is dead, only allow restart and auto-advance time.
    if player_dead.is_some() {
        for message in messages.read() {
            if let KeyCode::Char('r') = message.code {
                restart_requested.0 = true;
            }
        }
        // Auto-advance turns when spectating after death so the world keeps running.
        if spectating.0 {
            let awaiting = turn_state.as_ref().is_some_and(|s| *s.get() == TurnState::AwaitingInput);
            if awaiting {
                if let Some(ref mut nts) = next_turn_state {
                    nts.set(TurnState::PlayerTurn);
                }
            }
        }
        return;
    }

    let awaiting_input = turn_state
        .as_ref()
        .is_some_and(|s| *s.get() == TurnState::AwaitingInput);

    // Spectating mode removed — death screen only offers restart.

    // ── ESC menu input mode ─────────────────────────────────────
    if input_state.mode == InputMode::EscMenu {
        for message in messages.read() {
            match message.code {
                KeyCode::Char('q') => {
                    // Resume game (Q toggles ESC menu)
                    input_state.mode = InputMode::Game;
                    if *game_state.get() == GameState::Paused {
                        next_game_state.set(GameState::Playing);
                    }
                }
                KeyCode::Char('r') => {
                    // Restart
                    input_state.mode = InputMode::Game;
                    restart_requested.0 = true;
                }
                _ => {}
            }
        }
        return;
    }

    // ── Normal game input mode ──────────────────────────────────
    for message in messages.read() {
        // Dismiss the welcome screen on any key press.
        if input_state.welcome_visible {
            input_state.welcome_visible = false;
            continue; // consume the key that dismissed the welcome
        }

        // Exhaustive input handling — every arm here corresponds to a KEYBINDINGS entry.
        match message.code {
            // ── Q key: toggle ESC menu ──────────────────────────
            KeyCode::Char('q') => {
                // Open ESC menu and pause the game.
                input_state.mode = InputMode::EscMenu;
                if *game_state.get() == GameState::Playing {
                    next_game_state.set(GameState::Paused);
                }
            }
            // ── Cursor movement (IJKL) — advances one tick ─────
            KeyCode::Char('i') if awaiting_input => {
                move_cursor(&mut cursor, 0, 1, &mut player_viewshed, &mut next_turn_state);
            }
            KeyCode::Char('k') if awaiting_input => {
                move_cursor(&mut cursor, 0, -1, &mut player_viewshed, &mut next_turn_state);
            }
            KeyCode::Char('j') if awaiting_input => {
                move_cursor(&mut cursor, -1, 0, &mut player_viewshed, &mut next_turn_state);
            }
            KeyCode::Char('l') if awaiting_input => {
                move_cursor(&mut cursor, 1, 0, &mut player_viewshed, &mut next_turn_state);
            }
            // ── Center cursor on player (C) — advances one tick ──
            KeyCode::Char('c') if awaiting_input => {
                cursor.pos = player_pos.as_grid_vec();
                mark_viewshed_dirty(&mut player_viewshed);
                advance_turn(&mut next_turn_state);
            }
            // ── Auto-aim (V): move cursor one step toward nearest hostile — advances one tick ──
            KeyCode::Char('v') if awaiting_input => {
                let player_vec = player_pos.as_grid_vec();
                let mut best_dist = i32::MAX;
                let mut best_pos = None;
                for hostile_pos in &hostiles_query {
                    let hv = hostile_pos.as_grid_vec();
                    let dist = player_vec.chebyshev_distance(hv);
                    if dist < best_dist {
                        best_dist = dist;
                        best_pos = Some(hv);
                    }
                }
                if let Some(target) = best_pos {
                    let step = (target - cursor.pos).king_step();
                    cursor.pos += step;
                    mark_viewshed_dirty(&mut player_viewshed);
                    advance_turn(&mut next_turn_state);
                } else {
                    combat_log.push("No enemies visible.".into());
                }
            }
            // ── Movement keys (only while awaiting input) ───────
            // Normal movement — costs 3 ticks (physical movement is slower) and 2 stamina
            KeyCode::Char('w') | KeyCode::Up if awaiting_input => {
                extra_world_ticks.0 = 2;
                input_state.ability_stamina_pending = MOVE_STAMINA_COST;
                emit_move(&mut intents.move_intents, &mut next_turn_state, player_entity, 0, 1);
            }
            KeyCode::Char('s') | KeyCode::Down if awaiting_input => {
                extra_world_ticks.0 = 2;
                input_state.ability_stamina_pending = MOVE_STAMINA_COST;
                emit_move(&mut intents.move_intents, &mut next_turn_state, player_entity, 0, -1);
            }
            KeyCode::Char('a') | KeyCode::Left if awaiting_input => {
                extra_world_ticks.0 = 2;
                input_state.ability_stamina_pending = MOVE_STAMINA_COST;
                emit_move(&mut intents.move_intents, &mut next_turn_state, player_entity, -1, 0);
            }
            KeyCode::Char('d') | KeyCode::Right if awaiting_input => {
                extra_world_ticks.0 = 2;
                input_state.ability_stamina_pending = MOVE_STAMINA_COST;
                emit_move(&mut intents.move_intents, &mut next_turn_state, player_entity, 1, 0);
            }
            // ── Wait / skip turn (T) ────────────────────────────
            KeyCode::Char('t') if awaiting_input => {
                combat_log.push("You wait...".into());
                advance_turn(&mut next_turn_state);
            }
            // ── Reload weapon from inventory magazine — costs 6 ticks ──
            KeyCode::Char('r') if awaiting_input => {
                extra_world_ticks.0 = 5;
                input_state.reload_pending = true;
                advance_turn(&mut next_turn_state);
            }
            // ── Melee wide (roundhouse) attack — costs 2 ticks + stamina (F key) ────
            KeyCode::Char('f') if awaiting_input => {
                let has_stamina = player_stamina
                    .map(|m| m.current >= ROUNDHOUSE_STAMINA_COST)
                    .unwrap_or(false);
                if !has_stamina {
                    combat_log.push("Not enough stamina for roundhouse!".into());
                } else {
                    extra_world_ticks.0 = 1;
                    input_state.ability_stamina_pending = ROUNDHOUSE_STAMINA_COST;
                    intents.melee_wide_intents.write(MeleeWideIntent {
                        attacker: player_entity,
                    });
                    advance_turn(&mut next_turn_state);
                }
            }
            // ── Throw sand (G key): create sand cloud blocking vision (costs stamina) ──
            KeyCode::Char('g') if awaiting_input => {
                let has_stamina = player_stamina
                    .map(|m| m.current >= SAND_STAMINA_COST)
                    .unwrap_or(false);
                if !has_stamina {
                    combat_log.push("Not enough stamina!".into());
                } else {
                    let delta = cursor.pos - player_pos.as_grid_vec();
                    if delta == crate::grid_vec::GridVec::ZERO {
                        combat_log.push("Cursor is on your position!".into());
                    } else {
                        let step = delta.king_step();
                        let sand_center = player_pos.as_grid_vec() + step * 2;
                        // Create sand cloud as spell particles (visual obstruction)
                        intents.spell_intents.write(SpellCastIntent {
                            caster: player_entity,
                            radius: 2,
                            target: sand_center,
                            grenade_index: usize::MAX, // sentinel: no grenade consumed
                        });
                        input_state.ability_stamina_pending = SAND_STAMINA_COST;
                        combat_log.push("You throw a handful of sand!".into());
                        extra_world_ticks.0 = 0;
                        advance_turn(&mut next_turn_state);
                    }
                }
            }
            // ── Throw random item (E key): throw inventory item toward cursor (costs stamina) ──
            KeyCode::Char('e') if awaiting_input => {
                let has_stamina = player_stamina
                    .map(|m| m.current >= THROW_ITEM_STAMINA_COST)
                    .unwrap_or(false);
                if !has_stamina {
                    combat_log.push("Not enough stamina to throw!".into());
                } else {
                    input_state.ability_stamina_pending = THROW_ITEM_STAMINA_COST;
                    handle_throw_random(
                        player_entity, player_pos, player_inv, &item_kind_query,
                        &cursor, &mut intents, &mut extra_world_ticks,
                        &mut next_turn_state, &mut combat_log, &dynamic_rng, &seed,
                    );
                }
            }
            // ── Toggle God Mode (Shift+G) ───────────────────────
            KeyCode::Char('G') if awaiting_input => {
                god_mode.0 = !god_mode.0;
                if god_mode.0 {
                    combat_log.push("God mode ENABLED — you are invincible.".into());
                } else {
                    combat_log.push("God mode DISABLED.".into());
                }
            }
            // ── Use inventory item by slot (1-6) / Fire gun toward cursor / Throw / Grenade ──
            // Combat actions cost 2 ticks.
            KeyCode::Char(c @ '1'..='6') if awaiting_input => {
                let idx = (c as usize) - ('1' as usize);
                let mut handled = false;
                if let Some(inv) = player_inv
                    && let Some(&item_entity) = inv.items.get(idx)
                        && let Ok(kind) = item_kind_query.get(item_entity) {
                            if let ItemKind::Gun { loaded, name, .. } = kind {
                                if *loaded > 0 {
                                    let delta = cursor.pos - player_pos.as_grid_vec();
                                    if delta != crate::grid_vec::GridVec::ZERO {
                                        // Double-action revolvers (Starr 1858) cost only 1 tick.
                                        extra_world_ticks.0 = if name.contains("Starr") { 0 } else { 1 };
                                        intents.ranged_intents.write(RangedAttackIntent {
                                            attacker: player_entity,
                                            range: RANGED_ATTACK_RANGE,
                                            dx: delta.x,
                                            dy: delta.y,
                                            gun_item: Some(item_entity),
                                        });
                                        advance_turn(&mut next_turn_state);
                                        handled = true;
                                    } else {
                                        combat_log.push("Cursor is on your position!".into());
                                        handled = true;
                                    }
                                } else {
                                    combat_log.push("Gun is empty! Press R to reload.".into());
                                    handled = true;
                                }
                            } else if let ItemKind::Knife { attack, .. } | ItemKind::Tomahawk { attack, .. } = kind {
                                let delta = cursor.pos - player_pos.as_grid_vec();
                                if delta != crate::grid_vec::GridVec::ZERO {
                                    extra_world_ticks.0 = 1;
                                    intents.throw_item_intents.write(ThrowItemIntent {
                                        thrower: player_entity,
                                        item_entity,
                                        item_index: idx,
                                        dx: delta.x,
                                        dy: delta.y,
                                        range: crate::systems::projectile::THROWN_RANGE,
                                        damage: *attack,
                                    });
                                    advance_turn(&mut next_turn_state);
                                    handled = true;
                                } else {
                                    combat_log.push("Cursor is on your position!".into());
                                    handled = true;
                                }
                            } else if matches!(kind, ItemKind::Grenade { .. }) {
                                // Throw grenade from this inventory slot toward the cursor.
                                let has_stamina = player_stamina
                                    .map(|m| m.current >= SPELL_STAMINA_COST)
                                    .unwrap_or(false);
                                if !has_stamina {
                                    combat_log.push("Not enough stamina!".into());
                                } else {
                                    extra_world_ticks.0 = 1;
                                    intents.spell_intents.write(SpellCastIntent {
                                        caster: player_entity,
                                        radius: SPELL_RADIUS,
                                        target: cursor.pos,
                                        grenade_index: idx,
                                    });
                                    advance_turn(&mut next_turn_state);
                                }
                                handled = true;
                            } else if let ItemKind::Molotov { damage, radius, .. } = kind {
                                // Throw molotov from this inventory slot toward the cursor.
                                let has_stamina = player_stamina
                                    .map(|m| m.current >= SPELL_STAMINA_COST)
                                    .unwrap_or(false);
                                if !has_stamina {
                                    combat_log.push("Not enough stamina!".into());
                                } else {
                                    extra_world_ticks.0 = 1;
                                    intents.molotov_intents.write(MolotovCastIntent {
                                        caster: player_entity,
                                        radius: *radius,
                                        damage: *damage,
                                        target: cursor.pos,
                                        item_index: idx,
                                    });
                                    advance_turn(&mut next_turn_state);
                                }
                                handled = true;
                            }
                        }
                if !handled {
                    // Non-gun items: use normally.
                    intents.use_item_intents.write(UseItemIntent {
                        user: player_entity,
                        item_index: idx,
                    });
                    advance_turn(&mut next_turn_state);
                }
            }
            _ => {}
        }
    }
}

/// Reads keyboard input (windowed/WASM mode).
/// Uses Bevy's native `ButtonInput<KeyCode>` instead of crossterm events.
/// Mirrors the logic of `input_system` but maps Bevy key codes.
#[cfg(feature = "windowed")]
pub fn input_system_windowed(
    keys: Res<ButtonInput<bevy::input::keyboard::KeyCode>>,
    mut intents: IntentWriters,
    player_query: Query<(Entity, &Position, Option<&Stamina>, Option<&Inventory>, Option<&Dead>), With<PlayerControlled>>,
    mut player_viewshed: Query<&mut Viewshed, With<PlayerControlled>>,
    item_kind_query: Query<&ItemKind>,
    (hostiles_query, _health_query): (Query<&Position, (With<Faction>, Without<PlayerControlled>)>, Query<Entity, With<Health>>),
    game_state: Res<State<GameState>>,
    mut next_game_state: ResMut<NextState<GameState>>,
    turn_state: Option<Res<State<TurnState>>>,
    mut next_turn_state: Option<ResMut<NextState<TurnState>>>,
    mut combat_log: ResMut<CombatLog>,
    mut input_state: ResMut<InputState>,
    mut restart_requested: ResMut<RestartRequested>,
    mut cursor: ResMut<CursorPosition>,
    (mut extra_world_ticks, spectating, dynamic_rng, seed, _spatial): (ResMut<ExtraWorldTicks>, ResMut<SpectatingAfterDeath>, Res<DynamicRng>, Res<MapSeed>, Res<crate::resources::SpatialIndex>),
    mut god_mode: ResMut<crate::resources::GodMode>,
) {
    use bevy::input::keyboard::KeyCode as BevyKey;

    // Handle Dead and Victory states: R to restart, auto-advance turns when dead.
    if *game_state.get() == GameState::Dead || *game_state.get() == GameState::Victory {
        if keys.just_pressed(BevyKey::KeyR) {
            restart_requested.0 = true;
        }
        return;
    }

    let Ok((player_entity, player_pos, player_stamina, player_inv, player_dead)) = player_query.single() else {
        return;
    };

    // If the player is dead, only allow restart and auto-advance time.
    if player_dead.is_some() {
        if keys.just_pressed(BevyKey::KeyR) {
            restart_requested.0 = true;
        }
        if spectating.0 {
            let awaiting = turn_state.as_ref().is_some_and(|s| *s.get() == TurnState::AwaitingInput);
            if awaiting {
                if let Some(ref mut nts) = next_turn_state {
                    nts.set(TurnState::PlayerTurn);
                }
            }
        }
        return;
    }

    let awaiting_input = turn_state
        .as_ref()
        .is_some_and(|s| *s.get() == TurnState::AwaitingInput);

    // ── ESC menu input mode ─────────────────────────────────────
    if input_state.mode == InputMode::EscMenu {
        if keys.just_pressed(BevyKey::KeyQ) {
            input_state.mode = InputMode::Game;
            if *game_state.get() == GameState::Paused {
                next_game_state.set(GameState::Playing);
            }
        } else if keys.just_pressed(BevyKey::KeyR) {
            input_state.mode = InputMode::Game;
            restart_requested.0 = true;
        }
        return;
    }

    // ── Normal game input mode ──────────────────────────────────
    // Dismiss the welcome screen on any key press.
    if input_state.welcome_visible && keys.get_just_pressed().next().is_some() {
        input_state.welcome_visible = false;
        return; // consume the key that dismissed the welcome
    }

    // Helper: check if Shift is held (for Shift+G → God Mode toggle).
    let shift_held = keys.pressed(BevyKey::ShiftLeft) || keys.pressed(BevyKey::ShiftRight);

    // ── Q key: toggle ESC menu ──────────────────────────
    if keys.just_pressed(BevyKey::KeyQ) {
        input_state.mode = InputMode::EscMenu;
        if *game_state.get() == GameState::Playing {
            next_game_state.set(GameState::Paused);
        }
        return;
    }

    if !awaiting_input {
        return;
    }

    // ── Cursor movement (IJKL) — advances one tick ─────
    if keys.just_pressed(BevyKey::KeyI) {
        move_cursor(&mut cursor, 0, 1, &mut player_viewshed, &mut next_turn_state);
    } else if keys.just_pressed(BevyKey::KeyK) {
        move_cursor(&mut cursor, 0, -1, &mut player_viewshed, &mut next_turn_state);
    } else if keys.just_pressed(BevyKey::KeyJ) {
        move_cursor(&mut cursor, -1, 0, &mut player_viewshed, &mut next_turn_state);
    } else if keys.just_pressed(BevyKey::KeyL) {
        move_cursor(&mut cursor, 1, 0, &mut player_viewshed, &mut next_turn_state);
    }
    // ── Center cursor on player (C) ──
    else if keys.just_pressed(BevyKey::KeyC) {
        cursor.pos = player_pos.as_grid_vec();
        mark_viewshed_dirty(&mut player_viewshed);
        advance_turn(&mut next_turn_state);
    }
    // ── Auto-aim (V) ──
    else if keys.just_pressed(BevyKey::KeyV) {
        let player_vec = player_pos.as_grid_vec();
        let mut best_dist = i32::MAX;
        let mut best_pos = None;
        for hostile_pos in &hostiles_query {
            let hv = hostile_pos.as_grid_vec();
            let dist = player_vec.chebyshev_distance(hv);
            if dist < best_dist {
                best_dist = dist;
                best_pos = Some(hv);
            }
        }
        if let Some(target) = best_pos {
            let step = (target - cursor.pos).king_step();
            cursor.pos += step;
            mark_viewshed_dirty(&mut player_viewshed);
            advance_turn(&mut next_turn_state);
        } else {
            combat_log.push("No enemies visible.".into());
        }
    }
    // ── Movement keys (WASD / Arrows) — costs 3 ticks + 2 stamina ───────
    else if keys.just_pressed(BevyKey::KeyW) || keys.just_pressed(BevyKey::ArrowUp) {
        extra_world_ticks.0 = 2;
        input_state.ability_stamina_pending = MOVE_STAMINA_COST;
        emit_move(&mut intents.move_intents, &mut next_turn_state, player_entity, 0, 1);
    } else if keys.just_pressed(BevyKey::KeyS) || keys.just_pressed(BevyKey::ArrowDown) {
        extra_world_ticks.0 = 2;
        input_state.ability_stamina_pending = MOVE_STAMINA_COST;
        emit_move(&mut intents.move_intents, &mut next_turn_state, player_entity, 0, -1);
    } else if keys.just_pressed(BevyKey::KeyA) || keys.just_pressed(BevyKey::ArrowLeft) {
        extra_world_ticks.0 = 2;
        input_state.ability_stamina_pending = MOVE_STAMINA_COST;
        emit_move(&mut intents.move_intents, &mut next_turn_state, player_entity, -1, 0);
    } else if keys.just_pressed(BevyKey::KeyD) || keys.just_pressed(BevyKey::ArrowRight) {
        extra_world_ticks.0 = 2;
        input_state.ability_stamina_pending = MOVE_STAMINA_COST;
        emit_move(&mut intents.move_intents, &mut next_turn_state, player_entity, 1, 0);
    }
    // ── Wait / skip turn (T) ────────────────────────────
    else if keys.just_pressed(BevyKey::KeyT) {
        combat_log.push("You wait...".into());
        advance_turn(&mut next_turn_state);
    }
    // ── Reload weapon (R) — costs 6 ticks ──
    else if keys.just_pressed(BevyKey::KeyR) {
        extra_world_ticks.0 = 5;
        input_state.reload_pending = true;
        advance_turn(&mut next_turn_state);
    }
    // ── Melee wide / roundhouse (F) — costs 2 ticks + stamina ────
    else if keys.just_pressed(BevyKey::KeyF) {
        let has_stamina = player_stamina
            .map(|m| m.current >= ROUNDHOUSE_STAMINA_COST)
            .unwrap_or(false);
        if !has_stamina {
            combat_log.push("Not enough stamina for roundhouse!".into());
        } else {
            extra_world_ticks.0 = 1;
            input_state.ability_stamina_pending = ROUNDHOUSE_STAMINA_COST;
            intents.melee_wide_intents.write(MeleeWideIntent {
                attacker: player_entity,
            });
            advance_turn(&mut next_turn_state);
        }
    }
    // ── Throw sand (G without Shift) — costs stamina ──
    else if keys.just_pressed(BevyKey::KeyG) && !shift_held {
        let has_stamina = player_stamina
            .map(|m| m.current >= SAND_STAMINA_COST)
            .unwrap_or(false);
        if !has_stamina {
            combat_log.push("Not enough stamina!".into());
        } else {
            let delta = cursor.pos - player_pos.as_grid_vec();
            if delta == crate::grid_vec::GridVec::ZERO {
                combat_log.push("Cursor is on your position!".into());
            } else {
                let step = delta.king_step();
                let sand_center = player_pos.as_grid_vec() + step * 2;
                intents.spell_intents.write(SpellCastIntent {
                    caster: player_entity,
                    radius: 2,
                    target: sand_center,
                    grenade_index: usize::MAX,
                });
                input_state.ability_stamina_pending = SAND_STAMINA_COST;
                combat_log.push("You throw a handful of sand!".into());
                extra_world_ticks.0 = 0;
                advance_turn(&mut next_turn_state);
            }
        }
    }
    // ── Toggle God Mode (Shift+G) ───────────────────────
    else if keys.just_pressed(BevyKey::KeyG) && shift_held {
        god_mode.0 = !god_mode.0;
        if god_mode.0 {
            combat_log.push("God mode ENABLED — you are invincible.".into());
        } else {
            combat_log.push("God mode DISABLED.".into());
        }
    }
    // ── Throw random item (E) — costs stamina ──
    else if keys.just_pressed(BevyKey::KeyE) {
        let has_stamina = player_stamina
            .map(|m| m.current >= THROW_ITEM_STAMINA_COST)
            .unwrap_or(false);
        if !has_stamina {
            combat_log.push("Not enough stamina to throw!".into());
        } else {
            input_state.ability_stamina_pending = THROW_ITEM_STAMINA_COST;
            handle_throw_random(
                player_entity, player_pos, player_inv, &item_kind_query,
                &cursor, &mut intents, &mut extra_world_ticks,
                &mut next_turn_state, &mut combat_log, &dynamic_rng, &seed,
            );
        }
    }
    // ── Inventory slots (1-6) ──
    else {
        let digit_keys = [
            (BevyKey::Digit1, 0usize),
            (BevyKey::Digit2, 1),
            (BevyKey::Digit3, 2),
            (BevyKey::Digit4, 3),
            (BevyKey::Digit5, 4),
            (BevyKey::Digit6, 5),
        ];
        for &(key, idx) in &digit_keys {
            if keys.just_pressed(key) {
                let mut handled = false;
                if let Some(inv) = player_inv
                    && let Some(&item_entity) = inv.items.get(idx)
                        && let Ok(kind) = item_kind_query.get(item_entity) {
                            if let ItemKind::Gun { loaded, name, .. } = kind {
                                if *loaded > 0 {
                                    let delta = cursor.pos - player_pos.as_grid_vec();
                                    if delta != crate::grid_vec::GridVec::ZERO {
                                        extra_world_ticks.0 = if name.contains("Starr") { 0 } else { 1 };
                                        intents.ranged_intents.write(RangedAttackIntent {
                                            attacker: player_entity,
                                            range: RANGED_ATTACK_RANGE,
                                            dx: delta.x,
                                            dy: delta.y,
                                            gun_item: Some(item_entity),
                                        });
                                        advance_turn(&mut next_turn_state);
                                        handled = true;
                                    } else {
                                        combat_log.push("Cursor is on your position!".into());
                                        handled = true;
                                    }
                                } else {
                                    combat_log.push("Gun is empty! Press R to reload.".into());
                                    handled = true;
                                }
                            } else if let ItemKind::Knife { attack, .. } | ItemKind::Tomahawk { attack, .. } = kind {
                                let delta = cursor.pos - player_pos.as_grid_vec();
                                if delta != crate::grid_vec::GridVec::ZERO {
                                    extra_world_ticks.0 = 1;
                                    intents.throw_item_intents.write(ThrowItemIntent {
                                        thrower: player_entity,
                                        item_entity,
                                        item_index: idx,
                                        dx: delta.x,
                                        dy: delta.y,
                                        range: crate::systems::projectile::THROWN_RANGE,
                                        damage: *attack,
                                    });
                                    advance_turn(&mut next_turn_state);
                                    handled = true;
                                } else {
                                    combat_log.push("Cursor is on your position!".into());
                                    handled = true;
                                }
                            } else if matches!(kind, ItemKind::Grenade { .. }) {
                                let has_stamina = player_stamina
                                    .map(|m| m.current >= SPELL_STAMINA_COST)
                                    .unwrap_or(false);
                                if !has_stamina {
                                    combat_log.push("Not enough stamina!".into());
                                } else {
                                    extra_world_ticks.0 = 1;
                                    intents.spell_intents.write(SpellCastIntent {
                                        caster: player_entity,
                                        radius: SPELL_RADIUS,
                                        target: cursor.pos,
                                        grenade_index: idx,
                                    });
                                    advance_turn(&mut next_turn_state);
                                }
                                handled = true;
                            } else if let ItemKind::Molotov { damage, radius, .. } = kind {
                                let has_stamina = player_stamina
                                    .map(|m| m.current >= SPELL_STAMINA_COST)
                                    .unwrap_or(false);
                                if !has_stamina {
                                    combat_log.push("Not enough stamina!".into());
                                } else {
                                    extra_world_ticks.0 = 1;
                                    intents.molotov_intents.write(MolotovCastIntent {
                                        caster: player_entity,
                                        radius: *radius,
                                        damage: *damage,
                                        target: cursor.pos,
                                        item_index: idx,
                                    });
                                    advance_turn(&mut next_turn_state);
                                }
                                handled = true;
                            }
                        }
                if !handled {
                    intents.use_item_intents.write(UseItemIntent {
                        user: player_entity,
                        item_index: idx,
                    });
                    advance_turn(&mut next_turn_state);
                }
                break;
            }
        }
    }
}

/// Special ability: Throw random inventory item toward cursor.
fn handle_throw_random(
    player_entity: Entity,
    player_pos: &Position,
    player_inv: Option<&Inventory>,
    item_kind_query: &Query<&ItemKind>,
    cursor: &CursorPosition,
    intents: &mut IntentWriters,
    extra_world_ticks: &mut ExtraWorldTicks,
    next_turn_state: &mut Option<ResMut<NextState<TurnState>>>,
    combat_log: &mut CombatLog,
    dynamic_rng: &DynamicRng,
    seed: &MapSeed,
) {
    let delta = cursor.pos - player_pos.as_grid_vec();
    if delta == crate::grid_vec::GridVec::ZERO {
        combat_log.push("Cursor is on your position!".into());
        return;
    }

    let Some(inv) = player_inv else {
        combat_log.push("No inventory!".into());
        return;
    };

    if inv.items.is_empty() {
        combat_log.push("Inventory is empty!".into());
        return;
    }

    // Pick random item
    let idx = dynamic_rng.random_index(seed.0, 0x7000, inv.items.len());
    let item_entity = inv.items[idx];

    // Determine damage based on item type
    let damage = item_kind_query.get(item_entity).ok().map_or(2, |k| match k {
        ItemKind::Knife { attack, .. } | ItemKind::Tomahawk { attack, .. } => *attack,
        ItemKind::Gun { attack, .. } => *attack / 2,
        _ => 2,
    });

    intents.throw_item_intents.write(ThrowItemIntent {
        thrower: player_entity,
        item_entity,
        item_index: idx,
        dx: delta.x,
        dy: delta.y,
        range: crate::systems::projectile::THROWN_RANGE,
        damage,
    });
    extra_world_ticks.0 = 1;
    advance_turn(next_turn_state);
}

/// Helper: emits a `MoveIntent` and advances the turn state to `PlayerTurn`.
fn emit_move(
    move_intents: &mut MessageWriter<MoveIntent>,
    next_turn_state: &mut Option<ResMut<NextState<TurnState>>>,
    entity: Entity,
    dx: i32,
    dy: i32,
) {
    move_intents.write(MoveIntent { entity, dx, dy });
    advance_turn(next_turn_state);
}

/// Helper: transitions to `PlayerTurn`, ending the input phase.
#[inline]
fn advance_turn(next_turn_state: &mut Option<ResMut<NextState<TurnState>>>) {
    if let Some(next) = next_turn_state {
        next.set(TurnState::PlayerTurn);
    }
}

/// Helper: marks the player's viewshed as dirty so FOV is recalculated.
#[inline]
fn mark_viewshed_dirty(player_viewshed: &mut Query<&mut Viewshed, With<PlayerControlled>>) {
    if let Ok(mut vs) = player_viewshed.single_mut() {
        vs.dirty = true;
    }
}

/// Helper: moves the cursor by `(dx, dy)`, marks viewshed dirty, and advances the turn.
#[inline]
fn move_cursor(
    cursor: &mut ResMut<CursorPosition>,
    dx: i32,
    dy: i32,
    player_viewshed: &mut Query<&mut Viewshed, With<PlayerControlled>>,
    next_turn_state: &mut Option<ResMut<NextState<TurnState>>>,
) {
    cursor.pos.x += dx;
    cursor.pos.y += dy;
    mark_viewshed_dirty(player_viewshed);
    advance_turn(next_turn_state);
}
