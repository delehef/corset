#[macro_use]
extern crate pest_derive;
use clap_verbosity_flag::Verbosity;
use flate2::read::GzDecoder;
use is_terminal::IsTerminal;
use log::*;
use once_cell::sync::OnceCell;
use serde_json::Value;
use std::{
    fs::File,
    io::{BufReader, Cursor, Seek, Write},
    path::Path,
};

use clap::{Parser, Subcommand};
use color_eyre::eyre::*;

mod check;
mod column;
mod compiler;
mod compute;
mod expander;
mod exporters;
mod pretty;
mod utils;

#[derive(Default, Debug)]
struct Settings {
    pub full_trace: bool,
    pub trace_span: isize,
}

static SETTINGS: OnceCell<Settings> = OnceCell::new();

#[derive(Parser)]
#[clap(author, version)]
#[clap(propagate_version = true)]
pub struct Args {
    #[clap(flatten)]
    verbose: Verbosity,

    #[clap(
        help = "Either a file or a string containing the Corset code to process",
        global = true
    )]
    source: Vec<String>,

    #[clap(
        short = 't',
        long = "threads",
        help = "number of threds to use",
        default_value_t = 1,
        global = true
    )]
    threads: usize,

    #[clap(long = "no-stdlib")]
    no_stdlib: bool,

    #[clap(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Produce a Go-based constraint system
    Go {
        #[clap(
            short = 'C',
            long = "columns",
            help = "whether to render columns definition"
        )]
        render_columns: bool,

        #[clap(
            short = 'o',
            long = "constraints-file",
            help = "where to render the constraints"
        )]
        constraints_filename: Option<String>,

        #[clap(long = "columns-file", help = "where to render the columns")]
        columns_filename: Option<String>,

        #[clap(long = "assignment", default_value = "CE")]
        columns_assignment: String,

        #[clap(
            short = 'F',
            long = "function-name",
            value_parser,
            help = "The name of the function to be generated"
        )]
        fname: String,

        #[clap(
            short = 'P',
            long = "package",
            required = true,
            help = "In which package the function will be generated"
        )]
        package: String,
    },
    /// Produce a WizardIOP constraint system
    WizardIOP {
        #[clap(short = 'o', long = "out", help = "where to render the constraints")]
        out_filename: Option<String>,

        #[clap(
            short = 'P',
            long = "package",
            required = true,
            help = "In which package the function will be generated"
        )]
        package: String,
    },
    /// Produce a LaTeX file describing the constraints
    Latex {
        #[clap(
            short = 'o',
            long = "constraints-file",
            help = "where to render the constraints"
        )]
        constraints_filename: Option<String>,

        #[clap(long = "columns-file", help = "where to render the columns")]
        columns_filename: Option<String>,
    },
    /// Given a set of constraints and a trace file, fill the computed columns
    Compute {
        #[clap(
            short = 'T',
            long = "trace",
            required = true,
            help = "the trace to compute & verify"
        )]
        tracefile: String,

        #[clap(
            short = 'o',
            long = "out",
            help = "where to write the computed trace",
            required = true
        )]
        outfile: Option<String>,
    },
    /// Given a set of constraints, indefinitely check the traces from an SQL table
    CheckLoop {
        #[clap(long, default_value = "localhost")]
        host: String,
        #[clap(long, default_value = "postgres")]
        user: String,
        #[clap(long)]
        password: Option<String>,
        #[clap(long, default_value = "zkevm")]
        database: String,
        #[clap(long = "rm", help = "remove succesully validated blocks")]
        remove: bool,
    },
    /// Given a set of constraints, indefinitely fill the computed columns from/to an SQL table
    ComputeLoop {
        #[clap(long, default_value = "localhost")]
        host: String,
        #[clap(long, default_value = "postgres")]
        user: String,
        #[clap(long)]
        password: Option<String>,
        #[clap(long, default_value = "zkevm")]
        database: String,
    },
    /// Given a set of constraints and a filled trace, check the validity of the constraints
    Check {
        #[clap(
            short = 'T',
            long = "trace",
            required = true,
            help = "the trace to compute & verify"
        )]
        tracefile: String,

        #[clap(
            short = 'F',
            long = "trace-full",
            help = "print all the module columns on error"
        )]
        full_trace: bool,

        #[clap(long = "only", help = "only check these constraints")]
        only: Option<Vec<String>>,

        #[clap(short = 'S', long = "trace-span", help = "", default_value_t = 3)]
        trace_span: isize,
    },
    /// Given a set of Corset files, compile them into a single file for faster later use
    Compile {
        #[clap(
            short = 'o',
            long = "out",
            required = true,
            help = "compiled Corset file to create"
        )]
        outfile: String,
    },
}

fn read_trace<S: AsRef<str>>(tracefile: S) -> Result<Value> {
    let tracefile = tracefile.as_ref();
    info!("Parsing {}...", tracefile);
    let mut f = File::open(tracefile).with_context(|| format!("while opening `{}`", tracefile))?;

    let gz = GzDecoder::new(BufReader::new(&f));
    let v: Value = match gz.header() {
        Some(_) => serde_json::from_reader(gz),
        None => {
            f.rewind()?;
            serde_json::from_reader(BufReader::new(&f))
        }
    }
    .with_context(|| format!("while reading `{}`", tracefile))?;
    Ok(v)
}

fn main() -> Result<()> {
    let args = Args::parse();
    color_eyre::install()?;
    simplelog::TermLogger::init(
        args.verbose.log_level_filter(),
        simplelog::ConfigBuilder::new()
            .set_time_level(simplelog::LevelFilter::Off)
            .build(),
        simplelog::TerminalMode::Stderr,
        simplelog::ColorChoice::Auto,
    )?;
    let mut settings: Settings = Default::default();

    rayon::ThreadPoolBuilder::new()
        .num_threads(args.threads)
        .build_global()
        .unwrap();

    let (ast, mut constraints) = if args.source.len() == 1
        && Path::new(&args.source[0])
            .extension()
            .map(|e| e == "bin")
            .unwrap_or(false)
    {
        info!("Loading Corset binary...");
        (
            Vec::new(),
            ron::from_str(
                &std::fs::read_to_string(&args.source[0])
                    .with_context(|| eyre!("while reading `{}`", &args.source[0]))?,
            )
            .with_context(|| eyre!("while parsing `{}`", &args.source[0]))?,
        )
    } else {
        info!("Parsing Corset source files...");
        let mut inputs = vec![];
        if !args.no_stdlib {
            inputs.push(("stdlib", include_str!("stdlib.lisp").to_owned()));
        }
        for f in args.source.iter() {
            if std::path::Path::new(&f).is_file() {
                inputs.push((
                    f.as_str(),
                    std::fs::read_to_string(f).with_context(|| eyre!("reading `{}`", f))?,
                ));
            } else {
                inputs.push(("Immediate expression", f.into()));
            }
        }
        compiler::make(inputs.as_slice())?
    };

    match args.command {
        Commands::Go {
            constraints_filename,
            columns_filename,
            render_columns,
            package,
            columns_assignment,
            fname,
        } => {
            expander::expand_ifs(&mut constraints);
            let mut go_exporter = exporters::GoExporter {
                constraints_filename,
                package,
                ce: columns_assignment,
                render_columns,
                columns_filename,
                fname,
            };
            go_exporter.render(&constraints)?;
        }
        Commands::WizardIOP {
            out_filename,
            package,
        } => {
            expander::expand_ifs(&mut constraints);
            expander::expand(&mut constraints)?;
            let mut wiop_exporter = exporters::WizardIOP {
                out_filename,
                package,
            };
            wiop_exporter.render(&constraints)?;
        }
        Commands::Latex {
            constraints_filename,
            columns_filename,
        } => {
            let mut latex_exporter = exporters::LatexExporter {
                constraints_filename,
                columns_filename,
                render_columns: true,
            };
            latex_exporter.render(&ast)?
        }
        Commands::Compute { tracefile, outfile } => {
            let outfile = outfile.as_ref().unwrap();

            expander::expand(&mut constraints)?;
            compute::compute(
                &read_trace(&tracefile)?,
                &mut constraints,
                compute::PaddingStrategy::None,
            )
            .with_context(|| format!("while computing from `{}`", tracefile))?;

            let mut f = std::fs::File::create(&outfile)
                .with_context(|| format!("while creating `{}`", &outfile))?;

            constraints
                .write(&mut f)
                .with_context(|| format!("while writing to `{}`", &outfile))?;
        }
        Commands::ComputeLoop {
            host,
            user,
            password,
            database,
        } => {
            use flate2::write::GzEncoder;
            use flate2::Compression;

            expander::expand(&mut constraints)?;
            let mut db = utils::connect_to_db(&user, &password, &host, &database)?;

            loop {
                let mut local_constraints = constraints.clone();

                let mut tx = db.transaction()?;
                for row in tx.query(
                    "SELECT id, status, payload FROM blocks WHERE STATUS='to_corset' LIMIT 1 FOR UPDATE SKIP LOCKED",
                    &[],
                )? {
                    let id: &str = row.get(0);
                    let payload: &[u8] = row.get(2);
                    info!("Processing {}", id);

                    let v: Value = serde_json::from_str(
                        &utils::decompress(payload).with_context(|| "while decompressing payload")?,
                    )?;

                    compute::compute(&v, &mut local_constraints, compute::PaddingStrategy::None)
                        .with_context(|| "while computing columns")?;

                    let mut e = GzEncoder::new(Vec::new(), Compression::default());
                    local_constraints.write(&mut e)?;

                    tx.execute("UPDATE blocks SET payload=$1, status='to_prover' WHERE id=$2", &[&e.finish()?, &id])
                        .with_context(|| "while inserting back row")?;
                }
                tx.commit()?;

                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        Commands::CheckLoop {
            host,
            user,
            password,
            database,
            remove,
        } => {
            use flate2::write::GzEncoder;
            use flate2::Compression;

            let mut db = utils::connect_to_db(&user, &password, &host, &database)?;

            info!("Initiating waiting loop");
            loop {
                let mut local_constraints = constraints.clone();

                let mut tx = db.transaction()?;
                for row in tx.query(
                    "SELECT id, status, payload FROM blocks WHERE STATUS='to_corset' LIMIT 1 FOR UPDATE SKIP LOCKED",
                    &[],
                )? {
                    let id: &str = row.get(0);
                    let payload: &[u8] = row.get(2);
                    info!("Processing {}", id);

                    let gz = GzDecoder::new(Cursor::new(&payload));
                    let v: Value = match gz.header() {
                        Some(_) => serde_json::from_reader(gz),
                        None => {
                            serde_json::from_reader(Cursor::new(&payload))
                        }
                    }
                    .with_context(|| format!("while reading payload from {}", id))?;

                    compute::compute(
                        &v,
                        &mut constraints,
                        compute::PaddingStrategy::OneLine,
                    )
                        .with_context(|| format!("while expanding from {}", id))?;

                    match check::check(
                        &constraints,
                        &None,
                        args.verbose.log_level_filter() >= log::Level::Warn
                            && std::io::stdout().is_terminal(),
                    ) {
                        Ok(_) => {
                            if remove {
                                tx.execute("DELETE FROM blocks WHERE id=$1", &[&id])
                                    .with_context(|| "while inserting back row")?;
                            } else {
                                compute::compute(&v, &mut local_constraints, compute::PaddingStrategy::None)
                                    .with_context(|| "while computing columns")?;
                                let mut e = GzEncoder::new(Vec::new(), Compression::default());
                                local_constraints.write(&mut e)?;
                                tx.execute("UPDATE blocks SET payload=$1, status='to_prover' WHERE id=$2", &[&e.finish()?, &id])
                                    .with_context(|| "while inserting back row")?;
                            }
                        },
                        Err(_) => {
                            tx.execute("UPDATE blocks SET status='failed' WHERE id=$2", &[&id])
                                .with_context(|| "while inserting back row")?;
                        },
                    }

                }
                tx.commit()?;

                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        Commands::Check {
            tracefile,
            full_trace,
            trace_span,
            only,
        } => {
            settings.full_trace = full_trace;
            settings.trace_span = trace_span;
            SETTINGS.set(settings).unwrap();

            if utils::is_file_empty(&tracefile)? {
                warn!("`{}` is empty, exiting", tracefile);
                return Ok(());
            }

            compute::compute(
                &read_trace(&tracefile)?,
                &mut constraints,
                compute::PaddingStrategy::OneLine,
            )
            .with_context(|| format!("while expanding `{}`", tracefile))?;

            check::check(
                &constraints,
                &only,
                args.verbose.log_level_filter() >= log::Level::Warn
                    && std::io::stdout().is_terminal(),
            )
            .with_context(|| format!("while checking `{}`", tracefile))?;
            info!("{}: SUCCESS", tracefile)
        }
        Commands::Compile { outfile } => {
            std::fs::File::create(&outfile)
                .with_context(|| format!("while creating `{}`", &outfile))?
                .write_all(ron::to_string(&constraints).unwrap().as_bytes())
                .with_context(|| format!("while writing to `{}`", &outfile))?;
        }
    }

    Ok(())
}
