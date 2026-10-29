//! `GET /v1/usage/series` (docs/DESIGN.md §11): token use and cost day by day across
//! accounts, from the vendors' own logs.
//!
//! The day a figure belongs to depends on what the logs record. Claude Code, Codex and
//! CodeBuddy time every model call, so a series over them counts each call on the day it
//! was made (`granularity: request`), which also gives the number of calls and the biggest
//! one. A source whose logs hold only session totals falls back to counting each session on
//! the day it started (`granularity: session`), and `turns`/`max_request` are then null.
//! Days are local days: the machine's time zone decides where a call at 23:30 UTC belongs.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use crossagents_api_types::{
    UsageGranularity, UsageSeriesDay, UsageSeriesGroup, UsageSeriesGroupRow, UsageSeriesRange,
    UsageSeriesResponse, UsageSeriesSource, UsageSeriesTotal,
};
use crossagents_core::{SessionUsage, TokenTotals};

use crate::accounts::{ORIGIN_CROSSAGENTS, Owner};
use crate::prices;

/// What a series asks for, already parsed.
#[derive(Debug, Clone)]
pub(crate) struct SeriesQuery {
    pub(crate) range: UsageSeriesRange,
    pub(crate) group: UsageSeriesGroup,
    /// `all`, or one account id.
    pub(crate) source: String,
}

/// One account's logs, or why they could not be read. A failed source is left out of the
/// figures and named in `sources`; it never fails the whole series.
pub(crate) struct SourceData {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) provider: String,
    pub(crate) sessions: Result<Vec<SessionUsage>, String>,
}

/// Builds the series. `today` and `day_of` place calls on local days, so tests can pick the
/// zone.
pub(crate) fn build(
    query: &SeriesQuery,
    sources: &[SourceData],
    owners: &HashMap<String, Owner>,
    today: NaiveDate,
    day_of: &dyn Fn(DateTime<Utc>) -> NaiveDate,
) -> UsageSeriesResponse {
    // A series counts requests only when every session with use times them. Sources that
    // could not be read do not decide this: they are simply not part of the figures.
    let detailed = sources
        .iter()
        .filter_map(|source| source.sessions.as_ref().ok())
        .all(|sessions| {
            sessions
                .iter()
                .all(|s| used(&s.totals) == 0 || !s.requests.is_empty())
        });
    let granularity = if detailed {
        UsageGranularity::Request
    } else {
        UsageGranularity::Session
    };

    // `range=all` counts every recorded day; its start falls out of the walk.
    let start = match query.range {
        UsageSeriesRange::All => None,
        range => Some(today - Duration::days(range.days() - 1)),
    };
    let mut series = Series::new(query.group, start, today);

    for source in sources {
        let Ok(sessions) = &source.sessions else {
            continue;
        };
        for session in sessions {
            let group = session_group(query.group, session, owners);
            let mut counted = false;
            if detailed {
                for request in &session.requests {
                    let day = day_of(request.at);
                    counted |= series.add(
                        day,
                        group.as_ref(),
                        request.model.as_deref().or(session.model.as_deref()),
                        &request.totals,
                    );
                }
            } else {
                counted |= series.add(
                    day_of(session.started_at),
                    group.as_ref(),
                    session.model.as_deref(),
                    &session.totals,
                );
            }
            if counted {
                series.sessions += 1;
            }
        }
    }

    let first_day = series.first_day;
    let window: Vec<NaiveDate> = match (query.range, first_day) {
        (UsageSeriesRange::All, Some(first)) => dates(first, today),
        (UsageSeriesRange::All, None) => Vec::new(),
        (range, _) => dates(today - Duration::days(range.days() - 1), today),
    };
    let previous_total = match (query.range, start) {
        (UsageSeriesRange::All, _) | (_, None) => None,
        (_, Some(start)) => {
            let before = start - Duration::days(window.len() as i64);
            Some(series.tokens_between(before, start - Duration::days(1)))
        }
    };

    let days = window
        .iter()
        .map(|day| {
            let acc = series.days.get(day).cloned().unwrap_or_default();
            UsageSeriesDay {
                date: day.format("%Y-%m-%d").to_string(),
                cached_input: acc.cached,
                new_input: acc.new_input,
                output: acc.output,
                reasoning: acc.reasoning,
                cost_usd: acc.cost(),
            }
        })
        .collect();

    let mut groups: Vec<UsageSeriesGroupRow> = series
        .groups
        .into_iter()
        .map(|(key, group)| UsageSeriesGroupRow {
            key,
            label: group.label,
            cached_input: group.acc.cached,
            new_input: group.acc.new_input,
            output: group.acc.output,
            cost_usd: group.acc.cost(),
        })
        .collect();
    groups.sort_by(|a, b| {
        let tokens = |g: &UsageSeriesGroupRow| g.cached_input + g.new_input + g.output;
        tokens(b).cmp(&tokens(a)).then_with(|| a.key.cmp(&b.key))
    });

    let unpriced_models: Vec<String> = series.unpriced.into_iter().collect();
    let failed = sources.iter().filter(|s| s.sessions.is_err()).count();
    let notes = notes_of(granularity, &unpriced_models, failed, series.turns);

    UsageSeriesResponse {
        range: query.range,
        group: query.group,
        source: query.source.clone(),
        granularity,
        first_day: first_day.map(|day| day.format("%Y-%m-%d").to_string()),
        days,
        groups,
        total: UsageSeriesTotal {
            cached_input: series.total.cached,
            new_input: series.total.new_input,
            output: series.total.output,
            reasoning: series.total.reasoning,
            sessions: series.sessions,
            turns: (granularity == UsageGranularity::Request).then_some(series.turns),
            max_request: (granularity == UsageGranularity::Request).then_some(series.max_request),
            cost_usd: series.total.cost(),
        },
        previous_total,
        unpriced_models,
        sources: sources
            .iter()
            .map(|source| UsageSeriesSource {
                id: source.id.clone(),
                label: source.label.clone(),
                provider: source.provider.clone(),
                error: source.sessions.as_ref().err().cloned(),
            })
            .collect(),
        notes,
    }
}

/// In plain words: the granularity, how cost is counted, and what is left out.
fn notes_of(
    granularity: UsageGranularity,
    unpriced: &[String],
    failed: usize,
    turns: u64,
) -> Vec<String> {
    let mut notes = vec![match granularity {
        UsageGranularity::Request => format!(
            "Per request: every model call counts on the day it was made, in this machine's \
             local time zone (UTC{}); turns counts the calls and max_request is the largest \
             one's input plus output.",
            Local::now().offset()
        ),
        UsageGranularity::Session => "Per session: at least one source's logs do not time \
             every model call, so each session counts on the day it started, in this \
             machine's local time zone; turns and max_request are not available."
            .to_owned(),
    }];
    notes.push(
        "Costs use a built-in USD price table per million tokens, matched by model id \
         prefix; cache writes count as new input."
            .into(),
    );
    if !unpriced.is_empty() {
        notes.push(format!(
            "{} model(s) have no known price: their tokens are counted, their cost is not \
             (see unpriced_models).",
            unpriced.len()
        ));
    }
    if failed > 0 {
        notes.push(format!(
            "{failed} source(s) could not be read and are left out of the figures (see sources)."
        ));
    }
    if granularity == UsageGranularity::Request && turns == 0 {
        notes.push("No model call was recorded in this window.".into());
    }
    notes
}

/// A run of consecutive local days, both ends included.
fn dates(from: NaiveDate, to: NaiveDate) -> Vec<NaiveDate> {
    let mut days = Vec::new();
    let mut day = from;
    while day <= to {
        days.push(day);
        day += Duration::days(1);
    }
    days
}

fn used(totals: &TokenTotals) -> u64 {
    totals.input_tokens + totals.output_tokens
}

/// The key and label a session falls under, for the groups that describe the session rather
/// than the call; `None` for `group=total` and `group=model`, which are per row.
fn session_group(
    group: UsageSeriesGroup,
    session: &SessionUsage,
    owners: &HashMap<String, Owner>,
) -> Option<(String, String)> {
    let ours = session.originator.as_deref() == Some(ORIGIN_CROSSAGENTS);
    let owner = owners.get(&session.session_id);
    let outside = || {
        if ours {
            (
                "unknown".to_owned(),
                "(unknown CrossAgents agent)".to_owned(),
            )
        } else {
            ("outside".to_owned(), "Outside CrossAgents".to_owned())
        }
    };
    let pair = |key: String| (key.clone(), key);
    match group {
        UsageSeriesGroup::Total | UsageSeriesGroup::Model => None,
        UsageSeriesGroup::Origin => Some(if ours {
            ("crossagents".to_owned(), "CrossAgents".to_owned())
        } else {
            ("outside".to_owned(), "Outside CrossAgents".to_owned())
        }),
        UsageSeriesGroup::Agent => Some(match owner {
            Some(owner) => pair(owner.name.clone()),
            None => outside(),
        }),
        UsageSeriesGroup::Run => Some(match owner.and_then(|owner| owner.run) {
            Some(run) => pair(run.to_string()),
            None => outside(),
        }),
    }
}

/// Token counts of one day, one key, or the whole window.
#[derive(Debug, Clone, Default)]
struct Acc {
    cached: u64,
    new_input: u64,
    output: u64,
    reasoning: u64,
    cost: f64,
    priced: bool,
}

impl Acc {
    fn add(&mut self, cached: u64, new_input: u64, output: u64, reasoning: u64, cost: Option<f64>) {
        self.cached += cached;
        self.new_input += new_input;
        self.output += output;
        self.reasoning += reasoning;
        if let Some(cost) = cost {
            self.cost += cost;
            self.priced = true;
        }
    }

    /// `None` when nothing in it has a known price.
    fn cost(&self) -> Option<f64> {
        self.priced.then_some(self.cost)
    }
}

#[derive(Debug, Clone)]
struct GroupAcc {
    label: String,
    acc: Acc,
}

/// One series' running figures.
struct Series {
    group: UsageSeriesGroup,
    /// Days the figures count, both ends included; `None` counts every day.
    start: Option<NaiveDate>,
    end: NaiveDate,
    days: BTreeMap<NaiveDate, Acc>,
    /// Tokens per day, for any day, so the window before this one can be totalled.
    tokens_by_day: BTreeMap<NaiveDate, u64>,
    groups: BTreeMap<String, GroupAcc>,
    total: Acc,
    turns: u64,
    max_request: u64,
    unpriced: BTreeSet<String>,
    first_day: Option<NaiveDate>,
    sessions: u32,
}

impl Series {
    fn new(group: UsageSeriesGroup, start: Option<NaiveDate>, end: NaiveDate) -> Self {
        Self {
            group,
            start,
            end,
            days: BTreeMap::new(),
            tokens_by_day: BTreeMap::new(),
            groups: BTreeMap::new(),
            total: Acc::default(),
            turns: 0,
            max_request: 0,
            unpriced: BTreeSet::new(),
            first_day: None,
            sessions: 0,
        }
    }

    /// Counts one row: a request, or a whole session when the logs do not time its calls.
    /// Returns whether the row falls in the window.
    fn add(
        &mut self,
        day: NaiveDate,
        session_group: Option<&(String, String)>,
        model: Option<&str>,
        totals: &TokenTotals,
    ) -> bool {
        if day > self.end || used(totals) == 0 {
            return false;
        }
        let new_input = totals
            .input_tokens
            .saturating_sub(totals.cached_input_tokens);
        let tokens = totals.cached_input_tokens + new_input + totals.output_tokens;
        self.first_day = Some(self.first_day.map_or(day, |first| first.min(day)));
        *self.tokens_by_day.entry(day).or_default() += tokens;
        if self.start.is_some_and(|start| day < start) {
            return false;
        }
        // Reasoning is part of output: reported apart, never added again.
        let cost = match model.and_then(prices::price) {
            Some(price) => {
                Some(price.cost(totals.cached_input_tokens, new_input, totals.output_tokens))
            }
            None => {
                self.unpriced.insert(model.unwrap_or("unknown").to_owned());
                None
            }
        };
        self.days.entry(day).or_default().add(
            totals.cached_input_tokens,
            new_input,
            totals.output_tokens,
            totals.reasoning_output_tokens,
            cost,
        );
        self.total.add(
            totals.cached_input_tokens,
            new_input,
            totals.output_tokens,
            totals.reasoning_output_tokens,
            cost,
        );
        let row_group = match self.group {
            UsageSeriesGroup::Total => None,
            UsageSeriesGroup::Model => {
                let key = model.unwrap_or("unknown").to_owned();
                Some((key.clone(), key))
            }
            _ => session_group.cloned(),
        };
        if let Some((key, label)) = row_group {
            let group = self.groups.entry(key).or_insert_with(|| GroupAcc {
                label,
                acc: Acc::default(),
            });
            group.acc.add(
                totals.cached_input_tokens,
                new_input,
                totals.output_tokens,
                totals.reasoning_output_tokens,
                cost,
            );
        }
        // A request is one call; a session total stands for at least one.
        if totals.sessions == 0 {
            self.turns += 1;
            self.max_request = self
                .max_request
                .max(totals.input_tokens + totals.output_tokens);
        }
        true
    }

    /// Tokens counted on the days `from..=to`.
    fn tokens_between(&self, from: NaiveDate, to: NaiveDate) -> u64 {
        self.tokens_by_day.range(from..=to).map(|(_, n)| *n).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_core::RequestUsage;

    fn offset(hours: i32) -> chrono::FixedOffset {
        chrono::FixedOffset::east_opt(hours * 3600).unwrap()
    }

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn totals(input: u64, cached: u64, output: u64, reasoning: u64) -> TokenTotals {
        TokenTotals {
            sessions: 0,
            input_tokens: input,
            cached_input_tokens: cached,
            output_tokens: output,
            reasoning_output_tokens: reasoning,
            credits: 0.0,
        }
    }

    /// A session of one request, started at the same time.
    fn session(
        id: &str,
        started: &str,
        originator: Option<&str>,
        model: &str,
        totals: TokenTotals,
    ) -> SessionUsage {
        SessionUsage {
            session_id: id.into(),
            started_at: at(started),
            model: Some(model.into()),
            originator: originator.map(str::to_owned),
            totals: TokenTotals {
                sessions: 1,
                ..totals.clone()
            },
            requests: vec![RequestUsage {
                at: at(started),
                model: Some(model.into()),
                totals,
            }],
        }
    }

    fn source(id: &str, sessions: Result<Vec<SessionUsage>, String>) -> SourceData {
        SourceData {
            id: id.into(),
            label: id.into(),
            provider: "claude".into(),
            sessions,
        }
    }

    fn query(range: UsageSeriesRange, group: UsageSeriesGroup) -> SeriesQuery {
        SeriesQuery {
            range,
            group,
            source: "all".into(),
        }
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 30).unwrap()
    }

    fn run(
        group: UsageSeriesGroup,
        range: UsageSeriesRange,
        sources: &[SourceData],
    ) -> UsageSeriesResponse {
        build(
            &query(range, group),
            sources,
            &HashMap::new(),
            today(),
            &|t| t.date_naive(),
        )
    }

    #[test]
    fn buckets_requests_by_local_day_and_fills_the_empty_days() {
        let mut s = session(
            "s1",
            "2026-10-27T10:00:00Z",
            Some("crossagents"),
            "claude-sonnet-4-5",
            totals(100, 40, 10, 3),
        );
        // One session, two more calls: one late on the 27th UTC (= the 28th at UTC+8), one
        // on the 30th.
        s.requests.push(RequestUsage {
            at: at("2026-10-27T18:30:00Z"),
            model: Some("claude-sonnet-4-5".into()),
            totals: totals(200, 100, 20, 0),
        });
        s.requests.push(RequestUsage {
            at: at("2026-10-30T01:00:00Z"),
            model: Some("claude-sonnet-4-5".into()),
            totals: totals(50, 0, 5, 0),
        });
        s.totals = totals(350, 140, 35, 3);
        let sources = [source("a", Ok(vec![s]))];

        // UTC: the 27th and the 30th; the 28th and 29th are present with zeros.
        let series = run(UsageSeriesGroup::Total, UsageSeriesRange::Days7, &sources);
        assert_eq!(series.granularity, UsageGranularity::Request);
        assert_eq!(series.first_day.as_deref(), Some("2026-10-27"));
        assert_eq!(series.days.len(), 7);
        let day = |date: &str| series.days.iter().find(|d| d.date == date).unwrap();
        assert_eq!(day("2026-10-27").cached_input, 140);
        assert_eq!(day("2026-10-27").new_input, 160);
        assert_eq!(day("2026-10-27").output, 30);
        assert_eq!(day("2026-10-28").cached_input, 0);
        assert_eq!(day("2026-10-28").cost_usd, None);
        assert_eq!(day("2026-10-30").output, 5);
        assert_eq!(series.days.first().unwrap().date, "2026-10-24");
        assert_eq!(series.total.sessions, 1);
        assert_eq!(series.total.turns, Some(3));
        // Biggest call: 200 in + 20 out.
        assert_eq!(series.total.max_request, Some(220));

        // UTC+8: the 18:30 call falls on the 28th instead.
        let shifted = build(
            &query(UsageSeriesRange::Days7, UsageSeriesGroup::Total),
            &sources,
            &HashMap::new(),
            today(),
            &|t| t.with_timezone(&offset(8)).date_naive(),
        );
        let shifted_day = |date: &str| shifted.days.iter().find(|d| d.date == date).unwrap();
        assert_eq!(shifted_day("2026-10-27").cached_input, 40);
        assert_eq!(shifted_day("2026-10-28").cached_input, 100);
        assert_eq!(shifted.total.cached_input, 140);
    }

    #[test]
    fn a_source_without_timed_calls_falls_back_to_session_days() {
        let timed = session(
            "s1",
            "2026-10-30T09:00:00Z",
            None,
            "gpt-5",
            totals(1000, 0, 100, 0),
        );
        let mut coarse = session(
            "s2",
            "2026-10-29T09:00:00Z",
            None,
            "gpt-5",
            totals(500, 0, 50, 0),
        );
        coarse.requests.clear();
        let sources = [source("a", Ok(vec![timed])), source("b", Ok(vec![coarse]))];
        let series = run(UsageSeriesGroup::Total, UsageSeriesRange::Days7, &sources);
        assert_eq!(series.granularity, UsageGranularity::Session);
        assert_eq!(series.total.turns, None);
        assert_eq!(series.total.max_request, None);
        assert!(series.notes.iter().any(|n| n.contains("Per session")));
        // The coarse session counts on its start day.
        let day29 = series.days.iter().find(|d| d.date == "2026-10-29").unwrap();
        assert_eq!(day29.new_input, 500);
        assert_eq!(series.total.sessions, 2);
    }

    #[test]
    fn the_previous_window_is_the_same_length() {
        let old = session(
            "s1",
            "2026-10-23T09:00:00Z",
            None,
            "gpt-5",
            totals(100, 0, 10, 0),
        );
        let new = session(
            "s2",
            "2026-10-29T09:00:00Z",
            None,
            "gpt-5",
            totals(200, 0, 20, 0),
        );
        let sources = [source("a", Ok(vec![old, new]))];
        let series = run(UsageSeriesGroup::Total, UsageSeriesRange::Days7, &sources);
        // 2026-10-24..=2026-10-30 is the window, so the 23rd is the one before it.
        assert_eq!(series.previous_total, Some(110));
        // range=all has no window before it, and reaches back to the first recorded day.
        let all = run(UsageSeriesGroup::Total, UsageSeriesRange::All, &sources);
        assert_eq!(all.previous_total, None);
        assert_eq!(all.first_day.as_deref(), Some("2026-10-23"));
        assert_eq!(all.total.new_input, 300);
    }

    #[test]
    fn prices_what_it_knows_and_reports_the_rest() {
        let known = session(
            "s1",
            "2026-10-30T09:00:00Z",
            Some("crossagents"),
            "claude-sonnet-4-5",
            totals(1_000_000, 0, 1_000_000, 0),
        );
        let unknown = session(
            "s2",
            "2026-10-30T10:00:00Z",
            Some("crossagents"),
            "hy4-preview",
            totals(1000, 0, 1000, 0),
        );
        let sources = [source("a", Ok(vec![known, unknown]))];
        let series = run(UsageSeriesGroup::Total, UsageSeriesRange::Days7, &sources);
        // $3 for the million input tokens and $15 for the million output tokens.
        let cost = series.total.cost_usd.unwrap();
        assert!((cost - 18.0).abs() < 1e-9, "{cost}");
        assert_eq!(series.unpriced_models, ["hy4-preview"]);
        assert!(series.notes.iter().any(|n| n.contains("no known price")));

        // A window with nothing priced has no cost at all.
        let only_unknown = [source(
            "a",
            Ok(vec![session(
                "s3",
                "2026-10-30T11:00:00Z",
                None,
                "glm-5.3",
                totals(10, 0, 10, 0),
            )]),
        )];
        let series = run(
            UsageSeriesGroup::Total,
            UsageSeriesRange::Days7,
            &only_unknown,
        );
        assert_eq!(series.total.cost_usd, None);
        assert!(series.days.iter().all(|d| d.cost_usd.is_none()));
    }

    #[test]
    fn groups_by_model_origin_agent_and_run() {
        let run_id = "0193f0aa-0000-7000-8000-000000000001".parse().unwrap();
        let mut owners = HashMap::new();
        owners.insert(
            "s1".to_owned(),
            Owner {
                name: "Luna".into(),
                role: crossagents_core::Role::Worker,
                run: Some(run_id),
            },
        );
        let ours = session(
            "s1",
            "2026-10-30T09:00:00Z",
            Some("crossagents"),
            "claude-haiku-4-5",
            totals(100, 0, 10, 0),
        );
        let theirs = session(
            "s2",
            "2026-10-30T09:00:00Z",
            Some("claude"),
            "claude-haiku-4-5",
            totals(200, 0, 20, 0),
        );
        let sources = [source("a", Ok(vec![ours, theirs]))];
        let group = |by: UsageSeriesGroup| {
            build(
                &query(UsageSeriesRange::Days7, by),
                &sources,
                &owners,
                today(),
                &|t| t.date_naive(),
            )
        };

        let models = group(UsageSeriesGroup::Model);
        assert_eq!(models.groups.len(), 1);
        assert_eq!(models.groups[0].key, "claude-haiku-4-5");
        assert_eq!(models.groups[0].new_input, 300);

        let origins = group(UsageSeriesGroup::Origin);
        let keys: Vec<&str> = origins.groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"crossagents") && keys.contains(&"outside"));
        assert_eq!(origins.groups[0].label, "Outside CrossAgents");

        let agents = group(UsageSeriesGroup::Agent);
        assert_eq!(
            agents
                .groups
                .iter()
                .find(|g| g.key == "Luna")
                .unwrap()
                .new_input,
            100
        );
        assert!(
            agents
                .groups
                .iter()
                .any(|g| g.key == "outside" && g.label == "Outside CrossAgents")
        );

        let runs = group(UsageSeriesGroup::Run);
        assert_eq!(
            runs.groups
                .iter()
                .find(|g| g.key == run_id.to_string())
                .unwrap()
                .new_input,
            100
        );
    }

    #[test]
    fn one_failing_source_does_not_spoil_the_rest() {
        let good = session(
            "s1",
            "2026-10-30T09:00:00Z",
            None,
            "gpt-5",
            totals(100, 0, 10, 0),
        );
        let sources = [
            source("good", Ok(vec![good])),
            source(
                "broken",
                Err("cannot read ~/.codex/sessions: denied".into()),
            ),
        ];
        let series = run(UsageSeriesGroup::Total, UsageSeriesRange::Days7, &sources);
        assert_eq!(series.total.new_input, 100);
        // The unreadable one does not drag the others down to session days.
        assert_eq!(series.granularity, UsageGranularity::Request);
        assert_eq!(series.sources.len(), 2);
        let broken = series.sources.iter().find(|s| s.id == "broken").unwrap();
        assert!(broken.error.as_deref().unwrap().contains("denied"));
        assert!(
            series
                .sources
                .iter()
                .any(|s| s.id == "good" && s.error.is_none())
        );
        assert!(series.notes.iter().any(|n| n.contains("1 source(s)")));
    }

    #[test]
    fn all_days_reach_back_to_the_first_recorded_day() {
        let old = session(
            "s1",
            "2026-09-01T09:00:00Z",
            None,
            "gpt-5",
            totals(10, 0, 1, 0),
        );
        let sources = [source("a", Ok(vec![old]))];
        let series = run(UsageSeriesGroup::Total, UsageSeriesRange::All, &sources);
        assert_eq!(series.days.first().unwrap().date, "2026-09-01");
        assert_eq!(series.days.last().unwrap().date, "2026-10-30");
        assert_eq!(series.days.len(), 60);
    }

    #[test]
    fn an_empty_window_is_still_a_full_run_of_days() {
        let series = run(UsageSeriesGroup::Total, UsageSeriesRange::Days30, &[]);
        assert_eq!(series.days.len(), 30);
        assert!(
            series
                .days
                .iter()
                .all(|d| d.new_input == 0 && d.cost_usd.is_none())
        );
        assert_eq!(series.first_day, None);
        assert_eq!(series.total.turns, Some(0));
        assert_eq!(series.total.cost_usd, None);
        assert!(series.groups.is_empty());
    }
}
