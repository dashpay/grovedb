//! Replays a recorded stream of GroveDB calls on two engines. See
//! `lignin_testkit::diff::history`.

use std::process::ExitCode;

#[cfg(grovedb_engine_seam)]
fn settings(arguments: &[String]) -> Result<lignin_testkit::diff::history::Settings, String> {
    use lignin_testkit::diff::history::Settings;

    let mut settings = Settings {
        keep_going: 1,
        progress_every: 5000,
        scratch: std::env::temp_dir(),
        ..Settings::default()
    };
    let mut stream = None;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| {
            arguments
                .next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match argument.as_str() {
            "--scratch" => settings.scratch = value("--scratch")?.into(),
            "--stop-height" => {
                settings.stop_height = Some(
                    value("--stop-height")?
                        .parse()
                        .map_err(|_| "--stop-height: a height")?,
                )
            }
            "--keep-going" => {
                settings.keep_going = value("--keep-going")?
                    .parse()
                    .map_err(|_| "--keep-going: a number")?
            }
            "--stores" => settings.stores = Some(value("--stores")?.into()),
            "--resume-after" => {
                settings.resume_after = Some(
                    value("--resume-after")?
                        .parse()
                        .map_err(|_| "--resume-after: a height")?,
                )
            }
            "--progress" => {
                settings.progress_every = value("--progress")?
                    .parse()
                    .map_err(|_| "--progress: a number of blocks")?
            }
            "--show-heights" => {
                for range in value("--show-heights")?.split(',') {
                    let (from, to) = range.split_once('-').unwrap_or((range, range));
                    let height = |text: &str| {
                        text.parse::<u64>()
                            .map_err(|_| format!("--show-heights: `{text}` is no height"))
                    };
                    let (from, to) = (height(from)?, height(to)?);
                    if from > to {
                        return Err(format!("--show-heights: `{range}` ends before it begins"));
                    }
                    settings.show_heights.push((from, to));
                }
            }
            "--watch" => {
                let file = value("--watch")?;
                let text =
                    std::fs::read_to_string(&file).map_err(|e| format!("--watch {file}: {e}"))?;
                settings.watch.extend(
                    text.lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_string),
                );
            }
            other if other.starts_with("--") => return Err(format!("unknown option {other}")),
            other => {
                if let Some(first) = stream.replace(other.to_string()) {
                    return Err(format!("two streams: `{first}` and `{other}`"));
                }
            }
        }
    }
    settings.stream = stream
        .ok_or("usage: lignin-history STREAM --scratch DIR [--stop-height N] [--show-heights A-B,C] [--watch FILE] [--keep-going N] [--progress N] [--stores DIR [--resume-after N]]")?
        .into();
    Ok(settings)
}

fn main() -> ExitCode {
    #[cfg(grovedb_engine_seam)]
    {
        use lignin_testkit::{
            diff::history::{replay, report},
            golden::run::STACK_BYTES,
        };

        let arguments: Vec<String> = std::env::args().skip(1).collect();
        let settings = match settings(&arguments) {
            Ok(settings) => settings,
            Err(why) => {
                eprintln!("{why}");
                return ExitCode::from(2);
            }
        };
        let run = std::thread::Builder::new()
            .stack_size(STACK_BYTES)
            .spawn(move || {
                let mut out = std::io::stdout();
                let summary = replay(&settings, &mut out)?;
                report(&summary, &mut out).map_err(|e| e.to_string())?;
                Ok::<bool, String>(summary.alike())
            })
            .expect("a thread for the replay")
            .join();
        match run {
            Ok(Ok(true)) => ExitCode::SUCCESS,
            Ok(Ok(false)) => ExitCode::from(1),
            Ok(Err(why)) => {
                eprintln!("{why}");
                ExitCode::from(2)
            }
            Err(_) => ExitCode::from(3),
        }
    }
    // Against the engine the golden corpus is pinned to there is one engine
    // and nothing to compare it with.
    #[cfg(not(grovedb_engine_seam))]
    {
        eprintln!("lignin-history needs a GroveDB with the Engine seam");
        ExitCode::from(2)
    }
}
