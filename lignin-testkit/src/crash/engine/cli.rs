//! The `lignin-crash engine` commands: run the campaign through the full
//! engine, run one seed again, wipe again and again, or plant a defect and
//! require the campaign to find it.

use std::{collections::BTreeMap, io::Write, path::PathBuf, time::Duration};

use super::campaign::{
    campaign, Act, Acted, Found, Planted, Report, RunReport, Script, Settings, Shape, WIPE_EARLIER,
    WIPE_EMPTY, WIPE_UNCHANGED,
};
use crate::crash::{campaign::Faults, check::Kind, observe::Sabotage};

/// How the commands are used.
pub const USAGE: &str = "\
usage: lignin-crash engine <command> [options]

commands:
  run       run random workloads of GroveDB on the LigninDB engine, with the
            three threads of each database at work, take a crash image at
            every write boundary, recover every state the crash plans leave
            and hold each to the blocks that were committed, until a
            violation is found or an end that the options name is reached
  seed      make the workload of one seed again
  wipe      the same campaign with lineages that wipe their database again
            and again: every state of a crash during a wipe is as the
            database was before it, or empty, or, where blocks before the
            wipe were not durable, at one of those
  planted   plant a defect and require the campaign to find it: the
            acceptance test of the campaign itself

options:
  --seed N            the seed of the first run (default: 1)
  --runs N            how many runs, each with the seed after the one before
                      (default: 1, or no bound with --points or --duration;
                      planted: how many runs a defect may take, default 12)
  --points N          run, wipe: end with the run that brings the distinct
                      crash states recovered to N
  --duration SECONDS  run, wipe: begin no run after this long
  --jobs N            how many runs are made at once (default: 1)
  --faults KIND       run, seed: the faults every run plants in the file
                      layer: none, syncs or all (default: by the seed, none
                      for two seeds in four, then syncs, then all). The runs
                      of wipe and planted plant none
  --ops N             the steps of one lineage (default: 60)
  --stream-ops N      the most operations taken of one stream (default: 60)
  --lineages N        the most lineages of one run: the first, and those
                      that go on from a state a crash left (default: 4)
  --depth N           how many crashes deep a lineage can be (default: 2)
  --burst N           the most steps between two pauses in which the crash
                      images are gone through (default: 6)
  --plans N           the most crash plans taken of one image, the two ends
                      always among them; 0 for all of those the campaign
                      picks (default: 24)
  --again N           one state in N goes on after it was recovered: it
                      takes a block, is closed and is opened again; 0 for
                      none (default: 16)
  --save DIR          write the files of a state in which a violation was
                      found to a directory under DIR; planted: of a run that
                      has a violation with no defect planted
  --defect NAME       planted: log-sync, new-log-sync, segment-sync,
                      table-sync, manifest-sync, dir-sync,
                      checkpoint-dir-sync, forgotten-write or split-wipe
                      (default: each in turn)
  --quiet             print no line for a run

The streams of a run are a function of its seed and of the options that
shape a run (--faults, --ops, --stream-ops, --burst). In the first lineage
of a run with no faults that finds no violation, the operations, their order
and the blocks they commit follow from the seed, the command and those
options alone. Where the changes that the background threads of a database
make to its files fall among the caller's is the machine's to say, and so is
which thread a planted fault fails and the crash state that a later lineage
begins with: a seed need not give the same crash states twice, nor the same
operations after a fault or in a later lineage. Where --save names a
directory, run, seed and wipe keep the files of each crash state in which
they find a violation, and planted those of a run with no defect planted: a
report says where they are, or why they could not be written. A violation of
a session has no crash state, and no files. run,
seed and wipe exit with 0 when nothing was found and with 1 when a violation
was, and wipe also with 1 when, of the crashes during the wipe of a grove
that held something, none left the database as it was or none left it
empty. planted exits with 0 when every defect was found and with 1 when one
was not. A defect is found by the first violation of its campaign that fits
it: of a kind the defect makes, in a lineage in which it acted and as its
act there asks. The run of that violation's seed with no defect planted,
which the command makes once for a seed, has to have found no violation.
That a violation fits is evidence that the defect made it, and no proof.
";

/// A defect that `planted` plants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Defect {
    /// Its name on the command line.
    pub name: &'static str,
    /// What is planted.
    pub planted: Planted,
    /// The kinds of violation that the defect makes: one of another kind
    /// does not fit it.
    pub kinds: &'static [Kind],
    /// Whether a violation fits it only where it is of a checkpoint
    /// ([`Found::checkpoint`]).
    pub of_a_checkpoint: bool,
}

impl Defect {
    /// Whether `found` fits this defect in a run in which it acted at
    /// `acts`: a violation of a kind the defect makes, which fits one of
    /// those acts ([`Act::shows_in`]), and is of a checkpoint where the
    /// defect is of the checkpoints.
    pub fn found_by(&self, found: &Found, acts: &[Act]) -> bool {
        self.kinds.contains(&found.violation.kind)
            && (found.checkpoint || !self.of_a_checkpoint)
            && acts
                .iter()
                .any(|act| self.does(act.what) && act.shows_in(found))
    }

    /// Whether `what` is what this defect does when it acts.
    fn does(&self, what: Acted) -> bool {
        matches!(
            (self.planted, what),
            (Planted::File(_), Acted::Lied { .. })
                | (Planted::ForgottenWrite, Acted::Forgot { .. })
                | (Planted::SplitWipe, Acted::Split { .. })
                | (Planted::ThreadPanic, Acted::Panicked { .. })
        )
    }
}

/// The defects `planted` knows.
///
/// | Name | The defect | What fits it |
/// | --- | --- | --- |
/// | `log-sync` | A sync of a log file after the one that made its header durable does nothing, in the database and in a checkpoint that is being made: a flush, a close and the creation of a checkpoint report blocks durable that are not. | A crash state of an image taken after the first such sync, other than the state of that image in which every unsynced change is kept, in which the database or a checkpoint is before a durable block, does not open or is not sound. |
/// | `new-log-sync` | No sync of a log file does anything, the first included. | The same. |
/// | `segment-sync` | No sync of a segment after its first does anything: a table checkpoint names blocks that are not durable. | The same. |
/// | `table-sync` | No sync of a table checkpoint file does anything. | The same. |
/// | `manifest-sync` | No sync of the temporary file of the database's manifest does anything: a manifest is renamed into place that is not durable. A checkpoint's manifest is synced as it is asked. | The same. |
/// | `dir-sync` | No sync of the database directory does anything: a manifest that was replaced can come back. | The same. |
/// | `checkpoint-dir-sync` | No sync of the directory that the checkpoints are made in does anything: a checkpoint is reported made whose name is not durable, and one is reported removed whose name is. | Such a crash state in which a checkpoint that was made holds no database or one that was deleted is there. |
/// | `forgotten-write` | The database that is not crashed misses the first write of each block. | One of the differences between the two databases that [`Found::differs`] lists, while a block is open of which a write was missed. |
/// | `split-wipe` | A wipe is made as two commits: a write, and the wipe. | A crash state of an image taken during a wipe whose database is at the version that the write of such a wipe made: neither what the database held before the wipe nor empty. |
///
/// **What `found` says.** That a violation fits a defect is evidence that
/// the defect made it, and no proof. The campaign does not work out what
/// the files would hold had every sync been made, and a crash that loses
/// a change can leave what a file layer leaves too that made them all. So
/// a violation of another cause that fits, in a run whose control finds
/// nothing, is taken for the defect's.
///
/// A sync of a value-log file that does nothing is among the defects of
/// the store's campaign and not here: only some workloads have values that
/// lie out of line, and a defect that most runs cannot show is no test of
/// the campaign.
pub const DEFECTS: [Defect; 9] = [
    file("log-sync", lost("wal/", 1)),
    file("new-log-sync", lost("wal/", 0)),
    file("segment-sync", lost("seg/", 1)),
    file("table-sync", lost("table/", 0)),
    file("manifest-sync", lost("/db/MANIFEST.tmp", 0)),
    file("dir-sync", Sabotage::LostDirSync("db")),
    Defect {
        name: "checkpoint-dir-sync",
        planted: Planted::File(Sabotage::LostDirSync("checkpoints")),
        kinds: &[Kind::NotRecovered, Kind::NotCommitted],
        of_a_checkpoint: true,
    },
    Defect {
        name: "forgotten-write",
        planted: Planted::ForgottenWrite,
        kinds: &[Kind::Session],
        of_a_checkpoint: false,
    },
    Defect {
        name: "split-wipe",
        planted: Planted::SplitWipe,
        kinds: &[Kind::NotCommitted],
        of_a_checkpoint: false,
    },
];

/// What a state that a crash left can be found to be.
const CRASH: [Kind; 4] = [
    Kind::NotRecovered,
    Kind::NotCommitted,
    Kind::Inconsistent,
    Kind::Panicked,
];

const fn file(name: &'static str, sabotage: Sabotage) -> Defect {
    Defect {
        name,
        planted: Planted::File(sabotage),
        kinds: &CRASH,
        of_a_checkpoint: false,
    }
}

const fn lost(part: &'static str, after: u64) -> Sabotage {
    Sabotage::LostSync { part, after }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Run,
    Seed,
    Wipe,
    Planted,
}

#[derive(Clone, Debug)]
struct Invocation {
    command: Command,
    settings: Settings,
    defect: Option<Defect>,
    quiet: bool,
}

fn parse(arguments: &[String]) -> Result<Invocation, String> {
    let mut arguments = arguments.iter();
    let command = match arguments.next().map(String::as_str) {
        Some("run") => Command::Run,
        Some("seed") => Command::Seed,
        Some("wipe") => Command::Wipe,
        Some("planted") => Command::Planted,
        Some(other) => return Err(format!("`{other}` is not a command of engine")),
        None => return Err("no command given".to_string()),
    };
    let mut settings = Settings {
        runs: None,
        ..Settings::default()
    };
    let mut shape = Shape::default();
    let (mut defect, mut quiet) = (None, false);
    while let Some(option) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .cloned()
                .ok_or_else(|| format!("`{option}` needs a value"))
        };
        let number = |value: String| {
            let parsed = match value.strip_prefix("0x") {
                Some(hex) => u64::from_str_radix(hex, 16),
                None => value.parse::<u64>(),
            };
            parsed.map_err(|_| format!("`{value}` is not a number"))
        };
        match option.as_str() {
            "--seed" => settings.seed = number(value()?)?,
            "--runs" => settings.runs = Some(number(value()?)?.max(1)),
            "--points" => settings.points = Some(number(value()?)?),
            "--duration" => settings.duration = Some(Duration::from_secs(number(value()?)?)),
            "--jobs" => settings.jobs = number(value()?)?.max(1) as usize,
            "--faults" => {
                let named = value()?;
                let all = [Faults::None, Faults::Syncs, Faults::All];
                let faults = all.into_iter().find(|faults| faults.name() == named);
                shape.faults = Some(faults.ok_or(format!("`{named}` is no kind of faults"))?);
            }
            "--ops" => shape.ops = number(value()?)?.max(1) as usize,
            "--stream-ops" => shape.stream_ops = number(value()?)?.max(1) as usize,
            "--lineages" => shape.lineages = number(value()?)?.max(1) as usize,
            "--depth" => shape.depth = number(value()?)? as usize,
            "--burst" => shape.burst = number(value()?)?.max(1),
            "--plans" => shape.max_plans = number(value()?)? as usize,
            "--again" => shape.again_one_in = number(value()?)?,
            "--save" => settings.save = Some(PathBuf::from(value()?)),
            "--defect" => {
                let named = value()?;
                let known = DEFECTS.into_iter().find(|defect| defect.name == named);
                defect = Some(known.ok_or(format!("`{named}` is no defect"))?);
            }
            "--quiet" => quiet = true,
            other => return Err(format!("`{other}` is not an option")),
        }
    }
    let bounded = settings.points.is_some() || settings.duration.is_some();
    match command {
        Command::Run | Command::Wipe if settings.runs.is_none() && !bounded => {
            settings.runs = Some(1);
        }
        Command::Seed => {
            if bounded || settings.runs.is_some() {
                return Err(
                    "seed makes one run: it takes no --runs, --points or --duration".to_string(),
                );
            }
            settings.runs = Some(1);
        }
        Command::Planted if bounded => {
            return Err("planted takes --runs, not --points or --duration".to_string());
        }
        _ => {}
    }
    if defect.is_some() && command != Command::Planted {
        return Err("`--defect` is an option of planted".to_string());
    }
    if command == Command::Wipe {
        shape.script = Script::Wipes;
    }
    // A wipe is shown, and a planted defect is to be found, by a database
    // that meets no fault: a failure of the engine is then a violation.
    if matches!(command, Command::Wipe | Command::Planted) {
        if shape.faults.is_some() {
            return Err("`--faults` is an option of run and seed".to_string());
        }
        shape.faults = Some(Faults::None);
    }
    settings.shape = shape;
    Ok(Invocation {
        command,
        settings,
        defect,
        quiet,
    })
}

fn progress<'o>(out: &'o mut dyn Write, quiet: bool) -> impl FnMut(&RunReport, u64) + 'o {
    move |run, points| {
        if !quiet {
            let _ = writeln!(
                out,
                "seed {} ({}): {} crash states, {} violations; {points} distinct states so far",
                run.seed,
                run.recipe,
                run.fingerprints.len(),
                run.violations.len()
            );
        }
    }
}

/// The command that makes the workload of `seed` again as a campaign of
/// `shape` made it. A lineage whose steps are a list is of no command:
/// the line says so.
fn again(seed: u64, shape: &Shape) -> String {
    let usual = Shape::default();
    let command = match shape.script {
        Script::Drawn => "seed",
        Script::Wipes => "wipe",
        Script::Listed(_) => {
            return format!("(no command: the steps of seed {seed} were a list of the caller's)");
        }
    };
    let mut line = format!("lignin-crash engine {command} --seed {seed}");
    if let (Some(faults), Script::Drawn) = (shape.faults, shape.script) {
        line += &format!(" --faults {}", faults.name());
    }
    for (option, value, usual) in [
        ("--ops", shape.ops as u64, usual.ops as u64),
        (
            "--stream-ops",
            shape.stream_ops as u64,
            usual.stream_ops as u64,
        ),
        ("--lineages", shape.lineages as u64, usual.lineages as u64),
        ("--depth", shape.depth as u64, usual.depth as u64),
        ("--burst", shape.burst, usual.burst),
        ("--plans", shape.max_plans as u64, usual.max_plans as u64),
        ("--again", shape.again_one_in, usual.again_one_in),
    ] {
        if value != usual {
            line += &format!(" {option} {value}");
        }
    }
    line
}

fn report(out: &mut dyn Write, report: &Report, shape: &Shape) {
    let _ = writeln!(out, "{report}");
    let seconds = report.elapsed.as_secs_f64().max(1e-9);
    let _ = writeln!(
        out,
        "{:.0} distinct crash states a second",
        report.points as f64 / seconds
    );
    if let Some(found) = report.violations.first() {
        let _ = writeln!(
            out,
            "to make the workload of that run again: {}",
            again(found.seed, shape)
        );
    }
}

/// What `wipe` says of its campaign, and whether it is what the command is
/// there to show: no violation, and crashes that left each of the two.
fn wipe_verdict(done: &Report) -> (bool, String) {
    let count = |name| done.tally.get(name);
    let (unchanged, empty) = (count(WIPE_UNCHANGED), count(WIPE_EMPTY));
    let said = format!(
        "{} wipes; of the crash plans of the crashes during a wipe, {unchanged} leave the \
         database as it was, {} leave a block before the wipe that was not durable, and {empty} \
         leave it empty",
        count("wipes made"),
        count(WIPE_EARLIER)
    );
    (done.clean() && unchanged > 0 && empty > 0, said)
}

/// What `planted` says of `defect`, and whether it was found: by the
/// first violation of `done` that fits it, where the control of that
/// violation's seed is clean. `done` is the defect's campaign, with the
/// places where the defect did what it is planted to do, and `control`
/// says whether the run of a seed with no defect planted found no
/// violation.
fn verdict(defect: &Defect, done: &Report, control: &mut dyn FnMut(u64) -> bool) -> (bool, String) {
    let name = defect.name;
    let Some(first) = done.violations.first() else {
        return (false, format!("{name}: NOT FOUND in {} runs", done.runs));
    };
    // The finding, the acts it is held to and the control are of one run:
    // the one that has a violation which fits the defect.
    let fitting = (done.violations.iter()).find(|found| defect.found_by(found, &done.acts));
    let (found, why) = match fitting {
        Some(found) if control(found.seed) => {
            let said = format!(
                "{name}: found in the run of seed {}, after {:.1} s: {}",
                found.seed,
                done.elapsed.as_secs_f64(),
                found.violation
            );
            return (true, said);
        }
        Some(found) => (
            found,
            "the run of that seed has a violation with no defect planted",
        ),
        None if !done.acts.iter().any(|act| act.seed == first.seed) => {
            (first, "the defect did nothing in that run")
        }
        None => (first, "none fits what the defect did"),
    };
    let said = format!(
        "{name}: NOT FOUND: the run of seed {} has a violation, and {why}: {found}",
        found.seed
    );
    (false, said)
}

/// The most runs a planted defect may take to be found, unless `--runs`
/// says another number.
pub const PLANTED_RUNS: u64 = 12;

/// Runs the command `arguments` name, writes what it did to `out`, and
/// returns the exit code.
pub fn run_command(arguments: &[String], out: &mut dyn Write) -> u8 {
    let invocation = match parse(arguments) {
        Ok(invocation) => invocation,
        Err(error) => {
            let _ = writeln!(out, "lignin-crash: {error}\n\n{USAGE}");
            return 2;
        }
    };
    let Invocation {
        command,
        mut settings,
        defect,
        quiet,
    } = invocation;
    match command {
        Command::Run | Command::Seed => {
            let done = campaign(&settings, &mut progress(out, quiet));
            report(out, &done, &settings.shape);
            u8::from(!done.clean())
        }
        Command::Wipe => {
            let done = campaign(&settings, &mut progress(out, quiet));
            report(out, &done, &settings.shape);
            let (shown, said) = wipe_verdict(&done);
            let _ = writeln!(out, "{said}");
            u8::from(!shown)
        }
        Command::Planted => {
            let defects: Vec<_> = match defect {
                Some(defect) => vec![defect],
                None => DEFECTS.to_vec(),
            };
            settings.runs = Some(settings.runs.unwrap_or(PLANTED_RUNS));
            // The states of what a planted defect makes are what is looked
            // for, and are not kept. Those of a violation that a run has
            // with no defect planted are.
            let clean = Settings {
                runs: Some(1),
                jobs: 1,
                ..settings.clone()
            };
            settings.save = None;
            let mut controls: BTreeMap<u64, bool> = BTreeMap::new();
            let mut said_of_controls = Vec::new();
            let mut control = |seed| {
                let mut run = || {
                    let settings = Settings {
                        seed,
                        ..clean.clone()
                    };
                    let done = campaign(&settings, &mut |_, _| {});
                    if !done.clean() {
                        said_of_controls.push(format!("with no defect planted: {done}"));
                    }
                    done.clean()
                };
                *controls.entry(seed).or_insert_with(&mut run)
            };
            let mut missed = 0;
            for defect in defects {
                settings.shape.planted = Some(defect.planted);
                let done = campaign(&settings, &mut |_, _| {});
                let (found, said) = verdict(&defect, &done, &mut control);
                let _ = writeln!(out, "{said}");
                missed += u32::from(!found);
            }
            for said in said_of_controls {
                let _ = writeln!(out, "{said}");
            }
            u8::from(missed > 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crash::{campaign::Tally, check::Violation, engine::model::Doing};

    fn arguments(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    fn said(line: &str) -> (u8, String) {
        let mut out = Vec::new();
        let code = run_command(&arguments(line), &mut out);
        (code, String::from_utf8(out).expect("text"))
    }

    /// A violation of lineage 1 of the run of seed 7, found after step 4:
    /// with a boundary, of a state that a crash left of image 60, by a
    /// plan that loses a change.
    fn found(kind: Kind, boundary: Option<(u64, u64)>) -> Found {
        Found {
            boundary,
            image: boundary.map(|_| 60),
            plan: Some("a plan".to_string()),
            at: Some("a commit of the caller's thread".to_string()),
            doing: boundary.map(|_| Doing::Commit),
            trail: vec!["commit".to_string()],
            saved: Some(PathBuf::from("/states/seed-7")),
            ..Found::of(7, 1, 4, Violation::new(kind, "below the floor"))
        }
    }

    fn act(what: Acted) -> Act {
        Act {
            seed: 7,
            lineage: 1,
            what,
        }
    }

    /// The file layer of [`found`]'s lineage lied when fifty images had
    /// been taken of its file system.
    const LIED: Acted = Acted::Lied { images: 50 };

    fn with(violations: Vec<Found>) -> Report {
        Report {
            first_seed: 5,
            runs: 3,
            violations,
            acts: vec![act(LIED)],
            ..Report::default()
        }
    }

    #[test]
    fn the_options_set_the_campaign() {
        let invocation = parse(&arguments(
            "run --seed 0x10 --runs 3 --points 500 --duration 9 --jobs 2 --faults syncs --ops 7 \
             --stream-ops 20 --lineages 2 --depth 1 --burst 3 --plans 5 --again 0 --save /states \
             --quiet",
        ))
        .expect("parsed");
        let Settings {
            seed,
            runs,
            points,
            duration,
            jobs,
            shape,
            save,
        } = invocation.settings;
        assert_eq!((seed, runs, points, jobs), (16, Some(3), Some(500), 2));
        assert_eq!(duration, Some(Duration::from_secs(9)));
        assert_eq!(save, Some(PathBuf::from("/states")));
        let asked = Shape {
            ops: 7,
            stream_ops: 20,
            lineages: 2,
            depth: 1,
            burst: 3,
            max_plans: 5,
            again_one_in: 0,
            faults: Some(Faults::Syncs),
            ..Shape::default()
        };
        assert_eq!(shape, asked);
        assert!(invocation.quiet && invocation.command == Command::Run);

        // A run with no end named is one run of the usual shape, with the
        // faults of its seed, and one with an end has no bound on its runs.
        let usual = parse(&arguments("run")).expect("parsed");
        assert_eq!(usual.settings.runs, Some(1));
        assert_eq!(usual.settings.shape, Shape::default());
        assert!(!usual.quiet && usual.defect.is_none());
        let bounded = parse(&arguments("run --points 5")).expect("parsed");
        assert_eq!(bounded.settings.runs, None);
        let seed = parse(&arguments("seed --seed 4")).expect("parsed");
        assert_eq!((seed.settings.seed, seed.settings.runs), (4, Some(1)));
        assert_eq!(seed.command, Command::Seed);

        // The lineages of wipe have their own steps, and neither they nor
        // the runs of planted meet a fault.
        let wipe = parse(&arguments("wipe --duration 3")).expect("parsed");
        assert_eq!(wipe.settings.runs, None);
        assert_eq!(wipe.settings.shape.script, Script::Wipes);
        assert_eq!(wipe.settings.shape.faults, Some(Faults::None));
        let planted = parse(&arguments("planted --defect dir-sync --runs 2")).expect("parsed");
        assert_eq!(planted.defect.map(|defect| defect.name), Some("dir-sync"));
        assert_eq!(planted.settings.shape.script, Script::Drawn);
        assert_eq!(planted.settings.shape.faults, Some(Faults::None));
        assert_eq!(planted.settings.runs, Some(2));
        assert_eq!(
            parse(&arguments("planted")).expect("parsed").settings.runs,
            None
        );
    }

    #[test]
    fn what_is_not_a_command_line_is_refused_with_the_usage() {
        for line in [
            "",
            "crash",
            "run --seed",
            "run --seed x",
            "run --faults some",
            "run --fast",
            "run --defect log-sync",
            "wipe --faults all",
            "planted --faults syncs",
            "planted --defect none",
            "planted --points 4",
            "seed --runs 2",
            "seed --duration 2",
        ] {
            let (code, text) = said(line);
            assert_eq!(code, 2, "{line}");
            assert!(
                text.contains("usage: lignin-crash engine <command>"),
                "{line}: {text}"
            );
        }
    }

    #[test]
    fn the_defects_have_one_name_each_and_the_usage_names_them() {
        let names: Vec<_> = DEFECTS.iter().map(|defect| defect.name).collect();
        for (index, name) in names.iter().enumerate() {
            assert!(!names[..index].contains(name), "{name}");
            assert!(USAGE.contains(name), "{name}");
        }
        // The defects of the files show in a state that a crash left.
        for defect in &DEFECTS[..6] {
            assert!(matches!(defect.planted, Planted::File(_)), "{defect:?}");
            assert!(!defect.of_a_checkpoint && defect.kinds == CRASH);
        }
    }

    #[test]
    fn a_run_reports_what_it_did_and_exits_with_zero_when_it_is_clean() {
        assert_eq!(Faults::of_seed(1), Faults::None);
        let (code, text) = said("run --seed 1 --ops 6 --lineages 1 --plans 2 --again 0");
        assert_eq!(code, 0, "{text}");
        assert!(text.contains("seed 1 (grove version "), "{text}");
        assert!(text.contains("1 runs (seeds 1 to 1)"), "{text}");
        assert!(text.contains("distinct crash states a second"), "{text}");
        let (code, quiet) = said("seed --seed 1 --ops 6 --lineages 1 --plans 2 --again 0 --quiet");
        assert_eq!(code, 0, "{quiet}");
        assert!(!quiet.contains("seed 1 ("), "{quiet}");
        // The same seed with no fault, the same blocks.
        let digest = |text: &str| {
            text.lines()
                .find(|line| line.starts_with("digest of the blocks "))
                .map(str::to_string)
        };
        assert_eq!(digest(&text), digest(&quiet));
        assert!(digest(&text).is_some());
    }

    #[test]
    fn a_violation_comes_with_the_command_that_makes_its_workload_again() {
        let shape = Shape {
            faults: Some(Faults::Syncs),
            ops: 9,
            stream_ops: 20,
            burst: 3,
            max_plans: 5,
            again_one_in: 0,
            ..Shape::default()
        };
        let line = again(7, &shape);
        assert_eq!(
            line,
            "lignin-crash engine seed --seed 7 --faults syncs --ops 9 --stream-ops 20 --burst 3 \
             --plans 5 --again 0"
        );
        let command = |line: &str| {
            let command = line
                .strip_prefix("lignin-crash engine ")
                .expect("a command");
            parse(&arguments(command)).expect("parsed").settings
        };
        assert_eq!((command(&line).seed, command(&line).shape), (7, shape));
        // The faults of a seed are not named, nor what a wipe never has.
        assert_eq!(
            again(3, &Shape::default()),
            "lignin-crash engine seed --seed 3"
        );
        let wipes = Shape {
            script: Script::Wipes,
            faults: Some(Faults::None),
            lineages: 1,
            depth: 0,
            ..Shape::default()
        };
        let wipe = again(5, &wipes);
        assert_eq!(
            wipe,
            "lignin-crash engine wipe --seed 5 --lineages 1 --depth 0"
        );
        assert_eq!((command(&wipe).seed, command(&wipe).shape), (5, wipes));

        let done = with(vec![found(Kind::NotCommitted, Some((2, 90)))]);
        let mut out = Vec::new();
        report(&mut out, &done, &shape);
        let text = String::from_utf8(out).expect("text");
        for part in [
            "3 runs (seeds 5 to 7), 0 distinct crash states recovered, 1 violations",
            "VIOLATION: seed 7, lineage 1, after step 4, a crash at write boundary 90 of session 2: ",
            "  when the image was taken: a commit of the caller's thread\n",
            "  the crash kept: a plan\n",
            "  the files of the state: /states/seed-7\n",
            "  the last steps: commit\n",
        ] {
            assert!(text.contains(part), "{part}: {text}");
        }
        // A state that could not be written is said with the reason.
        let unwritten = Found {
            saved: None,
            not_saved: Some("/states/seed-7: no space".to_string()),
            ..found(Kind::NotCommitted, Some((2, 90)))
        };
        let said = unwritten.to_string();
        assert!(
            said.contains("  the files of the state were not written: /states/seed-7: no space\n"),
            "{said}"
        );
        assert!(
            text.ends_with(&format!("to make the workload of that run again: {line}\n")),
            "{text}"
        );
        // A campaign that found nothing names no run.
        let mut out = Vec::new();
        report(&mut out, &with(Vec::new()), &shape);
        let text = String::from_utf8(out).expect("text");
        assert!(text.ends_with("distinct crash states a second\n"), "{text}");
        let listed = Shape {
            script: Script::Listed(&[]),
            ..Shape::default()
        };
        assert!(again(5, &listed).starts_with("(no command"));
    }

    #[test]
    fn a_defect_is_found_by_a_violation_that_fits_what_it_did() {
        let log_sync = DEFECTS[0];
        assert_eq!(log_sync.name, "log-sync");
        let done = with(vec![found(Kind::NotCommitted, Some((1, 90)))]);
        let mut asked = Vec::new();
        let mut clean = |seed| {
            asked.push(seed);
            true
        };
        let (was, said) = verdict(&log_sync, &done, &mut clean);
        assert!(
            was && said.starts_with("log-sync: found in the run of seed 7, after "),
            "{said}"
        );
        assert!(said.ends_with("below the floor"), "{said}");
        assert_eq!(asked, [7]);

        // Not by a run in which it did nothing, nor by one of a seed that
        // has a violation without it.
        let nowhere = Report {
            acts: Vec::new(),
            ..done.clone()
        };
        let (was, said) = verdict(&log_sync, &nowhere, &mut |_| true);
        assert!(
            !was && said.contains("the defect did nothing in that run"),
            "{said}"
        );
        let (was, said) = verdict(&log_sync, &done, &mut |_| false);
        assert!(!was && said.contains("with no defect planted"), "{said}");
        // Not by a violation of another kind, nor, for a defect that shows
        // in a crash state, by one that is of no crash state.
        let session = with(vec![found(Kind::Session, None)]);
        let (was, said) = verdict(&log_sync, &session, &mut |_| true);
        assert!(!was, "{said}");
        let none_of_it = "log-sync: NOT FOUND: the run of seed 7 has a violation, and none fits";
        assert!(said.starts_with(none_of_it), "{said}");
        let not_crashed = with(vec![found(Kind::NotCommitted, None)]);
        assert!(!verdict(&log_sync, &not_crashed, &mut |_| true).0);
        // A later violation of the run can fit the defect.
        let both = with(vec![
            found(Kind::Session, None),
            found(Kind::Inconsistent, Some((1, 50))),
        ]);
        assert!(verdict(&log_sync, &both, &mut |_| true).0);
        // And not by a campaign that found nothing.
        let nothing = with(Vec::new());
        let (was, said) = verdict(&log_sync, &nothing, &mut |_| true);
        assert!(!was);
        assert_eq!(said, "log-sync: NOT FOUND in 3 runs");
    }

    /// With more than one run at a time, a campaign can hold the violations
    /// of two runs. The finding, the act and the control are of one run:
    /// the one that has a violation which fits the defect.
    #[test]
    fn the_control_is_of_the_run_whose_violation_fits_the_defect() {
        let log_sync = DEFECTS[0];
        // The run of seed 7 acted and has a violation of another kind;
        // that of seed 8 acted and has one that fits the defect.
        let of_seed_8 = Found {
            seed: 8,
            ..found(Kind::NotRecovered, Some((1, 90)))
        };
        let acted_in_8 = Act {
            seed: 8,
            ..act(LIED)
        };
        let done = Report {
            acts: vec![act(LIED), acted_in_8],
            ..with(vec![found(Kind::Session, None), of_seed_8.clone()])
        };
        let mut asked = Vec::new();
        let (was, said) = verdict(&log_sync, &done, &mut |seed| {
            asked.push(seed);
            seed == 8
        });
        assert!(was, "{said}");
        assert!(
            said.starts_with("log-sync: found in the run of seed 8, "),
            "{said}"
        );
        assert_eq!(asked, [8]);
        // A control of seed 8 that is not clean refuses it, whatever that
        // of seed 7 says.
        let (was, said) = verdict(&log_sync, &done, &mut |seed| seed == 7);
        assert!(!was, "{said}");
        assert!(said.contains("the run of seed 8 has a violation, and the run of that seed"));
        // The defect did nothing in the run of seed 7, and the violation
        // that fits it, in the run of seed 8, is found all the same.
        let only_in_8 = Report {
            acts: vec![acted_in_8],
            ..done.clone()
        };
        assert!(verdict(&log_sync, &only_in_8, &mut |seed| seed == 8).0);
    }

    /// What a violation has to be to fit an act, and what each defect
    /// asks beside.
    #[test]
    fn a_violation_fits_a_defect_by_what_its_act_asks() {
        let (log_sync, checkpoint, forgotten, split) =
            (DEFECTS[0], DEFECTS[6], DEFECTS[7], DEFECTS[8]);
        let of_image = |image: u64| Found {
            image: Some(image),
            ..found(Kind::NotCommitted, Some((1, 90)))
        };
        let change = |change: &dyn Fn(&mut Found)| {
            let mut found = of_image(60);
            change(&mut found);
            found
        };

        // A sync that was not made: a crash state of an image that was
        // taken after it, and of none that was taken before, the image
        // that was the last when the file layer lied among them; and
        // not the state with every unsynced change, which is what a
        // killed process leaves and shows no sync that was not made.
        let lied = [act(LIED)];
        assert!(log_sync.found_by(&of_image(51), &lied));
        assert!(!log_sync.found_by(&of_image(50), &lied));
        assert!(!log_sync.found_by(&change(&|found| found.whole = true), &lied));
        assert!(!log_sync.found_by(&of_image(60), &[]));
        assert!(!log_sync.found_by(&change(&|found| found.lineage = 0), &lied));
        assert!(!log_sync.found_by(&change(&|found| found.seed = 8), &lied));
        assert!(!log_sync.found_by(&found(Kind::NotCommitted, None), &lied));
        // The directory of the checkpoints: such a state in which a
        // checkpoint is not as the call left it that returned.
        assert!(checkpoint.of_a_checkpoint);
        assert!(!checkpoint.found_by(&of_image(60), &lied));
        assert!(checkpoint.found_by(&change(&|found| found.checkpoint = true), &lied));
        let lost = Found {
            violation: Violation::new(Kind::NotRecovered, "holds no database"),
            ..change(&|found| found.checkpoint = true)
        };
        assert!(checkpoint.found_by(&lost, &lied));

        // A write that the model missed: a difference between the two
        // databases while the block of that write is open.
        let forgot = [act(Acted::Forgot { attempt: 3 })];
        let differing = |attempt: u64, differs: bool| Found {
            attempt,
            differs,
            ..found(Kind::Session, None)
        };
        assert!(forgotten.found_by(&differing(3, true), &forgot));
        assert!(!forgotten.found_by(&differing(3, false), &forgot));
        assert!(!forgotten.found_by(&differing(4, true), &forgot));
        assert!(!forgotten.found_by(&differing(3, true), &lied));
        assert!(!log_sync.found_by(&of_image(60), &forgot));

        // A wipe that is two commits: a state of a crash during a wipe,
        // whose database is at the version that the first of them made.
        let halved = [act(Acted::Split { version: 12 })];
        let during_a_wipe = |version: Option<u64>| Found {
            doing: Some(Doing::Wipe),
            version,
            ..of_image(60)
        };
        assert!(split.found_by(&during_a_wipe(Some(12)), &halved));
        assert!(!split.found_by(&during_a_wipe(Some(13)), &halved));
        assert!(!split.found_by(&during_a_wipe(None), &halved));
        let during_a_commit = Found {
            version: Some(12),
            ..of_image(60)
        };
        assert!(!split.found_by(&during_a_commit, &halved));
        assert!(!split.found_by(&during_a_wipe(Some(12)), &lied));

        // A thread that panicked: a panic of a session, at or after it.
        let panicked = act(Acted::Panicked { op: 4 });
        let of_a_session = |kind: Kind| found(kind, None);
        assert!(panicked.shows_in(&of_a_session(Kind::Panicked)));
        assert!(!panicked.shows_in(&of_a_session(Kind::Session)));
        assert!(!act(Acted::Panicked { op: 5 }).shows_in(&of_a_session(Kind::Panicked)));
    }

    #[test]
    fn a_wipe_campaign_is_to_show_both_of_what_a_crash_leaves() {
        let counted = |unchanged, empty| {
            let mut tally = Tally::default();
            tally.count("wipes made", 3);
            tally.count(WIPE_UNCHANGED, unchanged);
            tally.count(WIPE_EARLIER, 1);
            tally.count(WIPE_EMPTY, empty);
            Report {
                tally,
                ..Report::default()
            }
        };
        let (shown, said) = wipe_verdict(&counted(2, 4));
        assert!(shown, "{said}");
        assert_eq!(
            said,
            "3 wipes; of the crash plans of the crashes during a wipe, 2 leave the database as \
             it was, 1 leave a block before the wipe that was not durable, and 4 leave it empty"
        );
        assert!(!wipe_verdict(&counted(0, 4)).0);
        assert!(!wipe_verdict(&counted(2, 0)).0);
        let mut violated = counted(2, 4);
        violated.violations = vec![found(Kind::NotCommitted, Some((1, 9)))];
        assert!(!wipe_verdict(&violated).0);
    }

    /// The first wipe of the script follows a flush: of the image that its
    /// commit makes, the state with no unsynced change is the database as
    /// it was, and the state with every change is the empty one.
    #[test]
    fn a_wipe_campaign_exits_with_zero_when_it_showed_both() {
        let (code, text) = said("wipe --ops 17 --lineages 1 --plans 4 --again 0 --quiet");
        assert_eq!(code, 0, "{text}");
        assert!(text.contains("\n2 wipes; of the crash plans"), "{text}");
        // A lineage that ends before its first wipe shows neither.
        let (code, text) = said("wipe --ops 6 --lineages 1 --plans 2 --again 0 --quiet");
        assert_eq!(code, 1, "{text}");
        assert!(text.contains("0 violations"), "{text}");
        assert!(text.contains("\n0 wipes; of the crash plans"), "{text}");
    }

    #[test]
    fn a_planted_defect_that_is_found_exits_with_zero() {
        let shape = "--ops 14 --stream-ops 40 --lineages 1 --burst 4 --plans 3 --again 0";
        let (code, text) = said(&format!("planted --defect forgotten-write {shape}"));
        assert_eq!(code, 0, "{text}");
        assert!(
            text.starts_with("forgotten-write: found in the run of seed 1, "),
            "{text}"
        );
    }
}
