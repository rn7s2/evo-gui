//! The empty tab's view model (§7.2, §9, §14.2): what the launch's controls show, the
//! plan they add up to, and the history list under them.
//!
//! The input is one JSON document: the `/catalog` body, exactly as
//! `evo-swarm catalog --json` prints it (§5.6, §9), and the answer to
//! `evo-swarm check --json` (§9). Nothing here does I/O and nothing here depends on
//! another crate — the app hands the bodies in, and the window paints what comes out.
//!
//! # The controls open on resolved values
//!
//! There is no "Default" anywhere. Every control opens on what evo would run **now**:
//!
//! * the coordinator's model — `check`'s own `model`, else `/catalog.default_model`,
//!   else the first registration the catalog says is `ready`;
//! * the coordinator's effort — `check`'s own `thinking`;
//! * the lanes' model — `check`'s `lane_model`, else the default when a lane can
//!   register it, else the first registration a lane can;
//! * the lanes' effort — `check`'s `lane_thinking`;
//! * the worker count — `check`'s own `workers`;
//!
//! # What a launch passes: only what a person set
//!
//! What the controls *show* is not what a launch passes. [`Launcher::plan`] carries a
//! value only for a control a **person** set — a model they picked, a slider they moved,
//! a count they typed — and passes nothing for the rest, which is how evo gets to resolve
//! them by its own chains (§9):
//!
//! * `check` is asked about the flags a launch would pass and nothing else, so handing it
//!   a value the page merely shows would be asking evo to confirm this app's own guess —
//!   the answer would be that guess, echoed back, and the page would show the app's
//!   numbers where evo's belong (the check would say `--workers 6` because the app said
//!   `--workers 6`, however many lanes evo would really run);
//! * before anyone touches a control, then, both the check and the launch carry no flags
//!   at all, and `check` answers with what evo itself resolves: its own model chain, its
//!   own effort chain, its own lane and worker defaults.
//!
//! A by-hand choice that happens to equal what evo resolved is passed like any other: it
//! asks for the same thing twice, which costs nothing, and it is what keeps a person's
//! pick theirs if evo's own answer later moves.
//!
//! A model is chosen by its **`ID@PROVIDER` pair**: `--model id@provider` selects
//! exactly that registration (§1), so two registrations of one id are two options, each
//! named with its provider. An option a lane cannot register is greyed out and says why
//! — the catalog's own `lanes.models[].ok`/`reason`, not a guess from an api set.
//!
//! # When `check` has not answered
//!
//! `check --json` resolves all five, by evo's own chains: the coordinator's effort from
//! its journal, then the agent's override, then the `thinking` setting, then `medium`
//! (and each model clamps what it is handed); a lane's from `--lane-thinking`, then what
//! a resumed swarm's record restored, then the coordinator's; the count from `--workers`,
//! then the `swarm-workers` setting, then 6.
//!
//! Before an answer is in hand — the first frame of a tab, or a check that failed — the
//! controls still have to show something, and it is evo's own last fallback: the middle
//! rung of the ladder and [`DEFAULT_WORKERS`]. Both are marked as such: the moment a
//! check answers, a control the launcher opened (nobody has touched it) is moved onto
//! what the check resolved, and a control the person moved is left alone.
//!
//! Those fallbacks are shown, never passed: a check that failed leaves every untouched
//! control out of the plan, so the launch carries no flag and evo runs its own default —
//! which is what a person who changed nothing asked for.
//!
//! # The effort ladder
//!
//! `/catalog.thinking_levels` is what a **session** accepts. `off` is the rung evo
//! retired (`+effort-levels+` is `low medium high xhigh max`, and both `--thinking` and
//! `/thinking` refuse anything else), and the catalog no longer lists it — but a body
//! out of the app's disk cache can predate that, and a launch flag cannot carry it
//! (`--thinking off` is a usage error), so the ladder the slider offers is whatever the
//! catalog listed without that rung.
//!
//! # History (§2)
//!
//! [`history_rows`] turns the session index's rows, merged with the app's own recents,
//! into display rows: the folder's own name, the `~`-shortened path, how long ago, and the
//! whole entry for the row's tooltip.

use serde_json::Value;

use crate::k_tokens;

/// The smallest and largest worker count a launch may carry (§14.2 allows 1–64).
pub const WORKERS_MIN: u16 = 1;
pub const WORKERS_MAX: u16 = 64;

/// The count `evo-swarm` starts with when neither `--workers` nor the project's
/// `swarm-workers` setting says otherwise (`swarm/main.lisp`).
pub const DEFAULT_WORKERS: u16 = 6;

/// The rung the effort sliders open on, and the level evo's own ladder puts in the
/// middle. Read as a name rather than an index: a catalog that lists its levels in
/// another order still opens on `medium`.
const MIDDLE_LEVEL: &str = "medium";

/// The rung evo retired. The catalog no longer lists it — a session folds it onto the
/// weakest live rung — but a body from the disk cache may predate that, and a launch flag
/// refuses it.
const RETIRED_LEVEL: &str = "off";

/// The thinking levels of §5.6, for a catalog that printed none: evo's own ladder.
const LADDER: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Which of the two cards a control belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// The coordinator: `--model`, `--thinking`.
    Coordinator,
    /// The lanes: `--lane-model`, `--lane-thinking`, `--workers`.
    Lanes,
}

impl Role {
    /// The two cards, in the order they are drawn.
    pub const ALL: [Role; 2] = [Role::Coordinator, Role::Lanes];

    /// What a field in this card is called.
    pub fn title(&self) -> &'static str {
        match self {
            Role::Coordinator => "Coordinator",
            Role::Lanes => "Workers",
        }
    }

    /// Where this card's own state sits in the two-element arrays below.
    fn slot(&self) -> usize {
        match self {
            Role::Coordinator => 0,
            Role::Lanes => 1,
        }
    }
}

/// One card's model field: the registration it shows, and who put it there.
///
/// The distinction is what keeps a person's own choice: a document that resolves another
/// registration moves a field the launcher resolved, and never one they picked — and it
/// is what a plan reads, since only a picked one becomes a flag.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Field {
    /// The registration the card shows, as `ID@PROVIDER`.
    key: Option<String>,
    /// The person picked this one, so the next document that resolves another leaves it
    /// be, and a launch passes it.
    by_hand: bool,
}

/// One registration in the catalog, as a model field offers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelOption {
    /// `ID@PROVIDER`: the value a model field holds, and what a flag takes.
    pub key: String,
    /// The registration's id, without its provider.
    pub id: String,
    /// The provider keyword, lower-cased.
    pub provider: String,
    /// The menu's second line: the context window, then what else the catalog says.
    pub detail: String,
    /// Whether evo can reach it now (`/catalog.models[].ready`).
    pub ready: bool,
    /// Why not, in evo's own words.
    pub ready_reason: Option<String>,
    /// Whether a **lane** can register it. Without a `lanes` list in the body this is
    /// the model's own `ready`: an `evo-agent` catalog has no such judgement to offer.
    pub lane_ok: bool,
    /// Why a lane cannot, in evo's own words.
    pub lane_reason: Option<String>,
}

impl ModelOption {
    /// Whether this card can launch on it as it stands.
    pub fn usable(&self, role: Role) -> bool {
        match role {
            Role::Coordinator => self.ready,
            Role::Lanes => self.lane_ok,
        }
    }

    /// Why this card cannot, in evo's own words where evo gave them.
    pub fn reason(&self, role: Role) -> Option<&str> {
        match role {
            Role::Coordinator => self.ready_reason.as_deref(),
            Role::Lanes => self.lane_reason.as_deref(),
        }
    }
}

/// What the controls add up to (§7.2, §1, §9): the flags a launch passes — and the flags
/// `evo-swarm check` is asked about, which are the same ones.
///
/// A field is `Some` only when a **person** set that control by hand: everything else is
/// `None`, so evo resolves it itself, exactly as the check reported it. A launch whose
/// plan [`is_default`](LaunchPlan::is_default) passes no flags at all.
///
/// This is a **new** swarm's plan. A swarm resumed from history takes the coordinator's
/// own record instead — its models are the session's, not the empty tab's — so the
/// workspace passes none of this for a [`HistoryRow`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchPlan {
    /// The coordinator's `--model`.
    pub model: Option<(String, String)>,
    /// The coordinator's `--thinking`.
    pub thinking: Option<String>,
    /// The `--workers` count.
    pub workers: Option<u16>,
    /// The lanes' `--lane-model`.
    pub lanes_model: Option<(String, String)>,
    /// The lanes' `--lane-thinking`.
    pub lane_thinking: Option<String>,
}

impl LaunchPlan {
    /// Whether this plan passes nothing at all — nobody has set a control by hand, so a
    /// launch leaves all five to evo and a check is asked about the bare launch.
    pub fn is_default(&self) -> bool {
        self.model.is_none()
            && self.thinking.is_none()
            && self.workers.is_none()
            && self.lanes_model.is_none()
            && self.lane_thinking.is_none()
    }

    /// The model pair as the `ID@PROVIDER` spec a `--model` / `--lane-model` flag takes.
    pub fn spec(model: &(String, String)) -> String {
        format!("{}@{}", model.0, model.1)
    }
}

// --- the model fields ---------------------------------------------------------------

/// Every registration a catalog lists, in the order evo listed them, as model options.
///
/// A registration without an id or a provider is not one: `--model id@provider` needs
/// both to name exactly what it means.
pub fn model_options(catalog: &Value) -> Vec<ModelOption> {
    let Some(models) = catalog.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    // `lanes.models` is the swarm's own judgement of what a lane can register; an
    // `evo-agent` catalog has none, and then a model's own `ready` answers.
    let lanes = catalog
        .get("lanes")
        .and_then(|lanes| lanes.get("models"))
        .and_then(Value::as_array);

    models
        .iter()
        .filter_map(|model| {
            let id = string(model, "id")?;
            let provider = string(model, "provider")?.to_lowercase();
            let ready = model.get("ready").and_then(Value::as_bool).unwrap_or(false);
            let lane = lanes.and_then(|lanes| {
                lanes.iter().find(|lane| {
                    string(lane, "id").as_deref() == Some(id.as_str())
                        && string(lane, "provider")
                            .map(|p| p.to_lowercase())
                            .as_deref()
                            == Some(provider.as_str())
                })
            });
            // Without a `lanes` list there is no judgement to offer, and the model's
            // own readiness answers for a lane too.
            let (lane_ok, lane_reason) = match (lanes, lane) {
                (Some(_), Some(lane)) => (
                    lane.get("ok").and_then(Value::as_bool).unwrap_or(false),
                    string(lane, "reason"),
                ),
                _ => (ready, None),
            };
            Some(ModelOption {
                key: format!("{id}@{provider}"),
                detail: model_detail(model),
                id,
                provider,
                ready,
                ready_reason: string(model, "reason"),
                lane_ok,
                lane_reason,
            })
        })
        .collect()
}

/// The levels a **launch flag** may carry, in the catalog's own order: everything
/// `/catalog.thinking_levels` lists but the retired `off` rung. A catalog that lists
/// nothing (or only the retired rung) falls back to evo's own ladder.
pub fn thinking_levels(catalog: &Value) -> Vec<String> {
    let listed: Vec<String> = catalog
        .get("thinking_levels")
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(Value::as_str)
                .filter(|level| *level != RETIRED_LEVEL)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if listed.is_empty() {
        LADDER.iter().map(|level| (*level).to_string()).collect()
    } else {
        listed
    }
}

/// One model's detail line: as much of the design's as the catalog can fill — the context
/// window, then the modalities — `200k ctx · vision`.
///
/// The design's own line ends with that model's effort range (`effort low–max`), which
/// `/catalog` does not publish: the global `thinking_levels` is the session's ladder, not
/// this model's. So it is left out rather than invented (`docs/api-gaps.md`); the
/// `reasoning` flag has no words of its own in the design and prints nothing.
fn model_detail(model: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(window) = model.get("context_window").and_then(Value::as_u64) {
        if window > 0 {
            parts.push(format!("{} ctx", k_tokens(window)));
        }
    }
    if model.get("images").and_then(Value::as_bool) == Some(true) {
        parts.push("vision".to_string());
    }
    parts.join(" · ")
}

// --- the empty tab's controls -------------------------------------------------------

/// The empty tab's controls: the two model fields, the two effort sliders, the worker
/// count and the history list.
///
/// Every control opens on a **resolved** value (§7.2): all five from `check --json`, or
/// from the catalog where the check has nothing to say (the two models), or — before a
/// check has answered — from evo's own last fallback (the ladder's middle rung and
/// [`DEFAULT_WORKERS`]). A value the launcher resolved is re-resolved whenever a document
/// arrives — the check is newer than the catalog, and the catalog newer than nothing —
/// while a control the person set themselves stays where they put it.
///
/// What a control *shows* and what a launch *passes* are two different things: only the
/// controls the person set are flags ([`Launcher::plan`]), and the ones the launcher
/// resolved are left to evo.
#[derive(Clone, Debug, Default)]
pub struct Launcher {
    /// The last `/catalog` body, and what `check --json` resolved the launch to: the
    /// two documents the fields are resolved from.
    check: Option<Value>,
    default_model: Option<(String, String)>,
    /// Every registration the last catalog listed.
    models: Vec<ModelOption>,
    /// The levels a launch flag may carry, weakest first.
    levels: Vec<String>,
    /// The two cards' model fields, the coordinator's first.
    fields: [Field; 2],
    /// Where each slider sits, as an index into [`Launcher::levels`], and whether a person
    /// moved it there — a moved slider is not re-resolved.
    efforts: [usize; 2],
    effort_by_hand: [bool; 2],
    /// The worker count, and whether a person typed it.
    workers: u16,
    count_by_hand: bool,
    history: Vec<HistoryRow>,
}

impl Launcher {
    /// A fresh tab: the ladder's own levels and evo's own worker count, and nothing to
    /// run yet — no catalog has arrived.
    pub fn new() -> Launcher {
        let levels = thinking_levels(&Value::Null);
        Launcher {
            efforts: [middle(&levels); 2],
            levels,
            workers: DEFAULT_WORKERS,
            ..Launcher::default()
        }
    }

    /// The model catalog: the `/catalog` body `evo-swarm catalog --json` prints (§5.6).
    /// Returns whether anything the window renders changed.
    pub fn set_catalog(&mut self, catalog: &Value) -> bool {
        let before = self.clone();
        self.models = model_options(catalog);
        self.levels = thinking_levels(catalog);
        self.default_model = catalog
            .get("default_model")
            .and_then(|default| Some((string(default, "id")?, string(default, "provider")?)))
            .map(|(id, provider)| (id, provider.to_lowercase()));
        // A ladder that got shorter (or a first one that arrived) moves the sliders onto
        // it; a rung that is still on it is left where it is.
        let end = self.levels.len().saturating_sub(1);
        for effort in &mut self.efforts {
            *effort = (*effort).min(end);
        }
        self.resolve();
        self.differs(&before)
    }

    /// What `evo-swarm check --json` resolved the launch to (§9): the models, and the
    /// words it would use if it could resolve none. Returns whether anything changed.
    pub fn set_check(&mut self, check: &Value) -> bool {
        let before = self.clone();
        self.check = Some(check.clone());
        self.resolve();
        self.differs(&before)
    }

    /// The resumable sessions, from the session index and the app's own recents (§2).
    /// `now` is the clock the relative times read against, `offset_seconds` the local UTC
    /// offset the rows are shown in, `home` the directory to shorten paths around.
    pub fn set_history(
        &mut self,
        entries: &[HistoryEntry],
        now: i64,
        offset_seconds: i32,
        home: Option<&str>,
    ) -> bool {
        let rows = history_rows(entries, now, offset_seconds, home);
        if self.history == rows {
            return false;
        }
        self.history = rows;
        true
    }

    pub fn history(&self) -> &[HistoryRow] {
        &self.history
    }

    /// Every registration a model field may offer, in the catalog's order.
    pub fn models(&self) -> &[ModelOption] {
        &self.models
    }

    /// One registration by its `ID@PROVIDER`.
    pub fn model(&self, key: &str) -> Option<&ModelOption> {
        self.models.iter().find(|model| model.key == key)
    }

    /// The levels the effort sliders offer, weakest first.
    pub fn levels(&self) -> &[String] {
        &self.levels
    }

    /// The registration one card is on, if it resolved to one.
    pub fn chosen(&self, role: Role) -> Option<&ModelOption> {
        self.model(self.chosen_key(role)?)
    }

    /// The `ID@PROVIDER` one card is on.
    pub fn chosen_key(&self, role: Role) -> Option<&str> {
        self.fields[role.slot()].key.as_deref()
    }

    /// What a model field shows when nothing resolved — evo's own words where it gave
    /// them, so the field says why it is empty rather than inventing a model.
    pub fn unresolved_note(&self, role: Role) -> Option<String> {
        if self.chosen(role).is_some() {
            return None;
        }
        self.check
            .as_ref()
            .and_then(|check| {
                check_reason(
                    check,
                    match role {
                        Role::Coordinator => "model",
                        Role::Lanes => "lane_model",
                    },
                )
            })
            .or_else(|| Some("No models are registered".to_string()))
    }

    /// Choose a registration — a person's own pick, which the launcher then leaves alone
    /// **and** the launch passes ([`Launcher::plan`]). An unknown key is ignored: the
    /// fields are rebuilt from the catalog, and a choice cannot outlive the option it
    /// named. Returns whether the choice changed anything the next launch would see —
    /// which includes putting a hand on the value the card already showed.
    pub fn choose(&mut self, role: Role, key: &str) -> bool {
        if self.model(key).is_none() {
            return false;
        }
        let field = &mut self.fields[role.slot()];
        // Picking what the card already shows is a choice like any other: the resolved
        // value was `None` in a plan, this one is a flag.
        let changed = !(field.by_hand && field.key.as_deref() == Some(key));
        field.key = Some(key.to_string());
        field.by_hand = true;
        changed
    }

    /// Where one card's effort slider sits.
    pub fn effort(&self, role: Role) -> usize {
        self.efforts[role.slot()]
    }

    /// The level one card's slider names.
    pub fn level(&self, role: Role) -> Option<&str> {
        self.levels.get(self.effort(role)).map(String::as_str)
    }

    /// Move one card's slider. An index past the ladder is clamped to it: the ladder can
    /// get shorter between one catalog and the next. Returns whether it moved.
    pub fn set_effort(&mut self, role: Role, index: usize) -> bool {
        if self.levels.is_empty() {
            return false;
        }
        let index = index.min(self.levels.len() - 1);
        let effort = &mut self.efforts[role.slot()];
        let moved = *effort != index;
        *effort = index;
        // A person's own rung, which a later check leaves alone.
        self.effort_by_hand[role.slot()] = true;
        moved
    }

    /// The worker count.
    pub fn workers(&self) -> u16 {
        self.workers
    }

    /// Set the worker count, clamped to what a launch may carry: typing a count outside
    /// 1–64 lands on the nearest end, the way the field's own stepper does. Returns
    /// whether it changed.
    pub fn set_workers(&mut self, count: u16) -> bool {
        let count = count.clamp(WORKERS_MIN, WORKERS_MAX);
        let typed = self.workers != count;
        self.workers = count;
        // As above: whoever typed a count keeps it.
        self.count_by_hand = true;
        typed
    }

    /// What the controls add up to (§7.2, §9): the flags a launch passes, and the flags a
    /// check is asked about.
    ///
    /// **Only a control a person set is in it.** A field nobody touched is `None`, so evo
    /// resolves it itself, by the same chains `check` reported — the values the page shows
    /// are evo's, and handing them back as flags would only ask evo to agree with itself.
    /// A launch whose plan is [`LaunchPlan::is_default`] passes no flag at all.
    pub fn plan(&self) -> LaunchPlan {
        LaunchPlan {
            model: self.by_hand_model(Role::Coordinator),
            thinking: self.by_hand_level(Role::Coordinator),
            workers: self.count_by_hand.then_some(self.workers),
            lanes_model: self.by_hand_model(Role::Lanes),
            lane_thinking: self.by_hand_level(Role::Lanes),
        }
    }

    /// The `ID@PROVIDER` pair one card contributes to a plan: its own choice, or nothing
    /// at all when the launcher resolved it.
    fn by_hand_model(&self, role: Role) -> Option<(String, String)> {
        self.fields[role.slot()]
            .by_hand
            .then(|| self.chosen(role).map(pair))
            .flatten()
    }

    /// The rung one card contributes to a plan, on the same rule.
    fn by_hand_level(&self, role: Role) -> Option<String> {
        self.effort_by_hand[role.slot()]
            .then(|| self.level(role).map(str::to_owned))
            .flatten()
    }

    /// Fill in every control from what evo resolved — and leave a person's own pick alone:
    /// only a control the launcher set itself is re-resolved.
    fn resolve(&mut self) {
        self.resolve_efforts();
        self.resolve_workers();
        for role in Role::ALL {
            let field = &self.fields[role.slot()];
            let stands = field.by_hand
                && field
                    .key
                    .as_deref()
                    .and_then(|key| self.model(key))
                    .is_some();
            if stands {
                continue;
            }
            self.fields[role.slot()] = Field {
                key: self.resolved(role),
                by_hand: false,
            };
        }
    }

    /// Where the two sliders sit: the effort `check` resolved for each card, or the middle
    /// rung while it has not answered.
    fn resolve_efforts(&mut self) {
        for role in Role::ALL {
            if self.effort_by_hand[role.slot()] {
                continue;
            }
            let resolved = self.check.as_ref().and_then(|check| {
                string(
                    check,
                    match role {
                        Role::Coordinator => "thinking",
                        Role::Lanes => "lane_thinking",
                    },
                )
            });
            self.efforts[role.slot()] = resolved
                .and_then(|level| self.levels.iter().position(|rung| *rung == level))
                .unwrap_or_else(|| middle(&self.levels));
        }
    }

    /// The count `check` resolved, or evo's own until it answers. A count off the wire is
    /// clamped to what a flag may carry, like the field's own typing.
    fn resolve_workers(&mut self) {
        if self.count_by_hand {
            return;
        }
        self.workers = self
            .check
            .as_ref()
            .and_then(|check| check.get("workers"))
            .and_then(Value::as_u64)
            .map(|count| u16::try_from(count).unwrap_or(WORKERS_MAX))
            .unwrap_or(DEFAULT_WORKERS)
            .clamp(WORKERS_MIN, WORKERS_MAX);
    }

    /// What evo resolves one card's model to, in the order §7.2 reads it.
    fn resolved(&self, role: Role) -> Option<String> {
        // What `check` resolved: its answer is the launch's own, `ok` or not — a model
        // it judged unusable is still the model it resolved, and the check's own line
        // under the cards says why.
        if let Some(key) = self
            .check
            .as_ref()
            .and_then(|check| checked_model(check, role))
        {
            if self.model(&key).is_some() {
                return Some(key);
            }
        }
        // Then evo's own default registration, when this card can run it.
        let default = self
            .default_model
            .as_ref()
            .map(|(id, provider)| format!("{id}@{provider}"));
        if let Some(key) =
            default.filter(|key| self.model(key).is_some_and(|model| model.usable(role)))
        {
            return Some(key);
        }
        // Then the first registration this card can run at all.
        self.models
            .iter()
            .find(|model| model.usable(role))
            .map(|model| model.key.clone())
    }

    /// Everything the empty tab renders. The history is not in it: it has its own entry
    /// point, from data that arrives separately (§2).
    fn differs(&self, before: &Launcher) -> bool {
        self.models != before.models
            || self.levels != before.levels
            || self.fields != before.fields
            || self.efforts != before.efforts
            || self.workers != before.workers
            || self.check != before.check
            || self.default_model != before.default_model
    }
}

fn pair(model: &ModelOption) -> (String, String) {
    (model.id.clone(), model.provider.clone())
}

/// Where a ladder opens: the middle rung by name, or the middle slot when a catalog
/// names its levels differently.
fn middle(levels: &[String]) -> usize {
    if levels.is_empty() {
        return 0;
    }
    levels
        .iter()
        .position(|level| level == MIDDLE_LEVEL)
        .unwrap_or(levels.len() / 2)
}

/// The registration `check --json` resolved for one card: `model` for the coordinator,
/// `lane_model` for the lanes. A field it left null resolved to nothing.
fn checked_model(check: &Value, role: Role) -> Option<String> {
    let entry = check.get(match role {
        Role::Coordinator => "model",
        Role::Lanes => "lane_model",
    })?;
    let id = string(entry, "id")?;
    match string(entry, "provider") {
        Some(provider) => Some(format!("{id}@{}", provider.to_lowercase())),
        None => Some(id),
    }
}

/// What `check --json` said about a field it could not resolve, in its own words: the
/// `reason` of the field itself, else the message of the problem about it.
fn check_reason(check: &Value, field: &str) -> Option<String> {
    if let Some(reason) = string(check.get(field)?, "reason") {
        return Some(reason);
    }
    check
        .get("problems")
        .and_then(Value::as_array)?
        .iter()
        .find(|problem| string(problem, "code").is_some_and(|code| code.contains(field)))
        .and_then(|problem| string(problem, "message"))
        .map(|message| one_line(&message))
}

/// A JSON field as a non-empty string.
fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A message with its newlines folded to spaces: one line, whatever evo wrote.
fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for ch in text.trim().chars() {
        if ch.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(ch);
        }
    }
    out
}

// --- the launch path ----------------------------------------------------------------

/// The folder's own path shortened around the home directory, the way the history rows
/// show it: `/Users/you/coding/foo` with home `/Users/you` → `~/coding/foo`.
pub fn home_short(path: &str, home: Option<&str>) -> String {
    // A folder reads without its trailing slash wherever it is (the index keeps a
    // cwd as `…/project/`), so `~/coding/evo` and `/tmp/run` look alike.
    let path = match path.trim_end_matches('/') {
        "" => path,
        trimmed => trimmed,
    };
    let Some(home) = home else {
        return path.to_string();
    };
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return path.to_string();
    }
    if path.trim_end_matches('/') == home {
        return "~".to_string();
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{}", rest.trim_end_matches('/')),
        _ => path.to_string(),
    }
}

// --- history (§2) -------------------------------------------------------------------

/// Where a history entry came from: the session index, or the app's own recent list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistorySource {
    /// Listed by `evo-agent sessions --json`.
    Index,
    /// Remembered by the app in `app.json`.
    Recent,
}

/// One resumable session, as the store hands it over: the journal's path and folder,
/// when it ran, what it ran with, and which list it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    /// Absolute path of the journal — the `--resume` argument.
    pub session_path: String,
    /// Absolute path of the folder the swarm ran in.
    pub folder: String,
    /// Last written, in epoch seconds.
    pub when: Option<i64>,
    /// The lane count the app remembers starting, when it remembers one.
    pub lanes: Option<u32>,
    pub coordinator_model: Option<String>,
    pub lanes_model: Option<String>,
    pub source: HistorySource,
    /// The app had this session open when it last quit. Only the app's own recents can
    /// say so — a swarm that never came down wrote no new journal — so a row the index
    /// alone found is always `false`.
    pub open_at_quit: bool,
}

/// One row of the history list (§7.2): the folder's own name, the folder's path shortened
/// around the home directory, how long ago it ran, and the whole entry for the row's
/// tooltip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRow {
    /// The folder's own name — the row's primary line. Not the index's `title`: see
    /// [`history_row`].
    pub title: String,
    /// The `~`-shortened path of the folder the swarm ran in.
    pub folder_short: String,
    /// How long ago, in the list's own words: `just now`, `2h ago`, `yesterday`, `3d
    /// ago`, `12 Sep`.
    pub when: String,
    /// The whole entry on one line, for the tooltip: the folder's full path, the session
    /// file, the instant with its zone spelled out, both models and the lane count.
    pub tooltip: String,
    /// The session to resume (`--resume`) and the folder to run in (cwd).
    pub session_path: String,
    pub folder: String,
    pub coordinator_model: Option<String>,
    pub lanes_model: Option<String>,
    pub source: HistorySource,
    /// The app had this session open when it last quit: the row wears a badge for it.
    pub open_at_quit: bool,
}

/// The history rows, newest first, with unknown parts left out.
///
/// `now` is the clock the relative times are read against (epoch seconds) and
/// `offset_seconds` the caller's local UTC offset, which the relative words use for their
/// day boundaries and the tooltip prints the instant in — a desktop shows a session's
/// time where the person is, not on the machine that wrote the journal.
pub fn history_rows(
    entries: &[HistoryEntry],
    now: i64,
    offset_seconds: i32,
    home: Option<&str>,
) -> Vec<HistoryRow> {
    let mut ordered: Vec<&HistoryEntry> = entries.iter().collect();
    // Newest first; a session whose time is unknown sorts last, and rows with the same
    // time keep the order they came in.
    ordered.sort_by_key(|entry| std::cmp::Reverse(entry.when));
    // One row per session: the index and the app's recents can both name it, and the app
    // is the only side that knows the models it ran with.
    let mut merged: Vec<HistoryEntry> = Vec::with_capacity(ordered.len());
    for entry in ordered {
        match merged
            .iter_mut()
            .find(|row| row.session_path == entry.session_path)
        {
            Some(base) => {
                base.lanes = base.lanes.or(entry.lanes);
                if base.coordinator_model.is_none() {
                    base.coordinator_model = entry.coordinator_model.clone();
                }
                if base.lanes_model.is_none() {
                    base.lanes_model = entry.lanes_model.clone();
                }
                base.open_at_quit |= entry.open_at_quit;
            }
            None => merged.push(entry.clone()),
        }
    }
    merged
        .iter()
        .map(|entry| history_row(entry, now, offset_seconds, home))
        .collect()
}

fn history_row(
    entry: &HistoryEntry,
    now: i64,
    offset_seconds: i32,
    home: Option<&str>,
) -> HistoryRow {
    HistoryRow {
        // The folder names the row. The index's own `title` is the first text of the
        // session's first user-role entry, and for a session evo started itself that text
        // is evo's — a goal's continuation prompt, a lane's brief — not something a person
        // wrote, so it cannot name the row (`docs/api-gaps.md` records what the index
        // would have to carry for a session's own words to be usable here).
        title: base_name(&entry.folder),
        folder_short: home_short(&entry.folder, home),
        when: entry
            .when
            .map(|when| relative_time(when, now, offset_seconds))
            .unwrap_or_default(),
        tooltip: tooltip_line(entry, offset_seconds),
        session_path: entry.session_path.clone(),
        folder: entry.folder.clone(),
        coordinator_model: entry.coordinator_model.clone(),
        lanes_model: entry.lanes_model.clone(),
        source: entry.source,
        open_at_quit: entry.open_at_quit,
    }
}

/// The row's tooltip: everything known about the session, in one line, with the instant in
/// the caller's own offset.
fn tooltip_line(entry: &HistoryEntry, offset_seconds: i32) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !entry.folder.is_empty() {
        // The full path, not the `~`-shortened one the row shows: the tooltip is where the
        // absolute answer belongs.
        parts.push(entry.folder.trim_end_matches('/').to_string());
    }
    let session = base_name(&entry.session_path);
    if !session.is_empty() {
        parts.push(session);
    }
    if let Some(when) = entry.when {
        parts.push(absolute_time(when, offset_seconds));
    }
    // The app's own record, right after the instant: this is the session you were in when
    // the app went away, not merely the one written last.
    if entry.open_at_quit {
        parts.push("open at last quit".to_string());
    }
    for (label, model) in [
        ("coordinator", entry.coordinator_model.as_deref()),
        ("lanes", entry.lanes_model.as_deref()),
    ] {
        if let Some(model) = model.filter(|model| !model.is_empty()) {
            parts.push(format!("{label}: {model}"));
        }
    }
    if let Some(lanes) = entry.lanes {
        parts.push(format!(
            "{} lane{}",
            lanes,
            if lanes == 1 { "" } else { "s" }
        ));
    }
    parts.join(" · ")
}

/// The last component of a path, without a trailing separator: a folder's own name, or a
/// session's file name.
fn base_name(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit('/').next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => trimmed.to_string(),
    }
}

/// How long ago, in the history list's words: `just now`, `5m ago`, `2h ago`, `yesterday`,
/// `3d ago`, then the date — `12 Sep`, or `12 Sep 2025` when it is another year.
///
/// `offset_seconds` is the local UTC offset: `yesterday` means the calendar day before the
/// caller's today (so an evening session is still yesterday the next morning, and one from
/// two evenings ago is not), and the dates are the caller's calendar dates. The elapsed-hour
/// words are plain elapsed time and need no zone.
///
/// A time in the future reads as `just now`: the clock the app compares against is the
/// same machine's.
pub fn relative_time(when: i64, now: i64, offset_seconds: i32) -> String {
    let elapsed = now - when;
    if elapsed < 60 {
        return "just now".to_string();
    }
    if elapsed < 3600 {
        return format!("{}m ago", elapsed / 60);
    }
    if elapsed < 86_400 {
        return format!("{}h ago", elapsed / 3600);
    }
    // Elapsed time is at least a day, so the local day is at least yesterday's: with a fixed
    // offset, `days` here is ≥ 1 by construction.
    let offset = offset_seconds as i64;
    let days = (now + offset).div_euclid(86_400) - (when + offset).div_euclid(86_400);
    if days == 1 {
        return "yesterday".to_string();
    }
    if days < 7 {
        return format!("{}d ago", days);
    }
    let (year, month, day) = civil_from_epoch(when + offset);
    let (this_year, ..) = civil_from_epoch(now + offset);
    if year == this_year {
        format!("{} {}", day, MONTHS[(month - 1) as usize])
    } else {
        format!("{} {} {}", day, MONTHS[(month - 1) as usize], year)
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Seconds since the epoch as a UTC calendar date.
fn civil_from_epoch(seconds: i64) -> (i64, u32, u32) {
    civil_from_days(seconds.div_euclid(86_400))
}

/// Days since 1970-01-01 as `(year, month, day)`, UTC — Howard Hinnant's civil calendar
/// algorithm, exact for every input (no float, no table).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let month_index = (5 * day_of_year + 2) / 153; // [0, 11], March is 0
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32; // [1, 31]
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// An instant in full, in a caller-given offset, with that offset spelled out:
/// `2026-09-29 17:25:44 +08:00`.
///
/// The offset is always printed, even when it is `+00:00`: the instant is epoch seconds and
/// a reader has to be told which zone the clock face is in, never left to assume one.
fn absolute_time(when: i64, offset_seconds: i32) -> String {
    let local = when + offset_seconds as i64;
    let (year, month, day) = civil_from_epoch(local);
    let seconds_of_day = local.rem_euclid(86_400);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {}",
        year,
        month,
        day,
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
        offset_label(offset_seconds)
    )
}

/// A UTC offset as `+08:00`, `-05:00`, `+05:30`.
fn offset_label(offset_seconds: i32) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let magnitude = offset_seconds.unsigned_abs();
    format!(
        "{}{:02}:{:02}",
        sign,
        magnitude / 3600,
        (magnitude % 3600) / 60
    )
}
