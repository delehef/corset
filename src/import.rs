use super::compiler::{ColumnRef, Magma};
use crate::column::Value as CValue;
use anyhow::*;
use cached::Cached;
use flate2::bufread::GzDecoder;
use log::*;
use logging_timer::time;
use owo_colors::OwoColorize;
#[cfg(not(all(target_arch = "x86_64", target_feature = "avx")))]
use serde_json::Value;
#[cfg(all(target_arch = "x86_64", target_feature = "avx"))]
use simd_json::BorrowedValue as Value;
#[cfg(all(target_arch = "x86_64", target_feature = "avx"))]
use std::io::Read;
use std::{
    fs::File,
    io::{BufReader, Seek},
};

use crate::{
    column::{Column, Register},
    compiler::ConstraintSet,
    pretty::Pretty,
    structs::Handle,
};

#[time("info", "Parsing trace from JSON file with SIMD")]
pub fn read_trace(tracefile: &str, cs: &mut ConstraintSet) -> Result<()> {
    let mut f = File::open(tracefile).with_context(|| format!("while opening `{}`", tracefile))?;

    #[cfg(all(target_arch = "x86_64", target_feature = "avx"))]
    {
        let mut content = Vec::new();
        let mut gz = GzDecoder::new(BufReader::new(&f));
        match gz.header() {
            Some(_) => gz.read_to_end(&mut content),
            None => {
                f.rewind()?;
                BufReader::new(&f).read_to_end(&mut content)
            }
        }
        .with_context(|| format!("while reading `{}`", tracefile))?;
        let v = simd_json::to_borrowed_value(&mut content)
            .map_err(|e| anyhow!("while parsing json: {}", e))?;
        fill_traces(&v, vec![], cs, &mut None).with_context(|| "while reading columns")
    }
    #[cfg(not(all(target_arch = "x86_64", target_feature = "avx")))]
    {
        let gz = GzDecoder::new(BufReader::new(&f));
        let v: Value = match gz.header() {
            Some(_) => serde_json::from_reader(gz),
            None => {
                f.rewind()?;
                serde_json::from_reader(BufReader::new(&f))
            }
        }
        .with_context(|| format!("while reading `{}`", tracefile))?;
        fill_traces(&v, vec![], cs, &mut None).with_context(|| "while reading columns")
    }
}

#[time("info", "Parsing trace from JSON with SIMD")]
pub fn read_trace_str(tracestr: &[u8], cs: &mut ConstraintSet) -> Result<()> {
    #[cfg(all(target_arch = "x86_64", target_feature = "avx"))]
    {
        let mut content = Vec::new();
        let mut gz = GzDecoder::new(BufReader::new(tracestr));
        match gz.header() {
            Some(_) => {
                gz.read_to_end(&mut content)?;
            }
            None => {
                content = tracestr.to_vec();
            }
        };
        let v = simd_json::to_borrowed_value(&mut content)
            .map_err(|e| anyhow!("while parsing json: {}", e))?;
        fill_traces(&v, vec![], cs, &mut None).with_context(|| "while reading columns")
    }
    #[cfg(not(all(target_arch = "x86_64", target_feature = "avx")))]
    {
        let gz = GzDecoder::new(BufReader::new(tracestr));
        let v: Value = match gz.header() {
            Some(_) => serde_json::from_reader(gz),
            None => serde_json::from_reader(BufReader::new(tracestr)),
        }?;
        fill_traces(&v, vec![], cs, &mut None).with_context(|| "while reading columns")
    }
}

#[cfg(not(all(target_arch = "x86_64", target_feature = "avx")))]
fn parse_column(xs: &[Value], h: &Handle, t: Magma) -> Result<Vec<CValue>> {
    let mut cache_num = cached::SizedCache::with_size(200000); // ~1.60MB cache
    let mut cache_str = cached::SizedCache::with_size(200000); // ~1.60MB cache
    let mut r = vec![CValue::zero()];
    let xs = xs
        .iter()
        .map(|x| match x {
            Value::Number(n) => t.rm().validate(
                cache_num
                    .cache_get_or_set_with(n, || CValue::from(n.as_str()))
                    .to_owned(),
            ),
            Value::String(s) => t.rm().validate(
                cache_str
                    .cache_get_or_set_with(s.clone(), || CValue::from(s.as_str()))
                    .to_owned(),
            ),
            _ => bail!("expected numeric value, found `{}`", x),
        })
        .collect::<Result<Vec<_>>>()?;

    if let Err(msg) = crate::utils::maybe_warn(t, &r, h) {
        error!("{}", msg);
    };
    r.extend(xs);
    Ok(r)
}

#[cfg(all(target_arch = "x86_64", target_feature = "avx"))]
fn parse_column(xs: &[Value], h: &Handle, t: Magma) -> Result<Vec<CValue>> {
    let mut cache = cached::SizedCache::with_size(200000); // ~1.60MB cache
    let mut r = vec![CValue::zero()];
    let xs = xs
        .iter()
        .map(|x| {
            let s = match x {
                Value::Static(n) => match n {
                    simd_json::StaticNode::I64(i) => i.to_string(),
                    simd_json::StaticNode::U64(i) => i.to_string(),
                    _ => {
                        unreachable!()
                    }
                },
                Value::String(s) => s.to_string(),
                _ => bail!("expected numeric value, found `{}`", x),
            };
            t.rm().validate(
                cache
                    .cache_get_or_set_with(s.clone(), || CValue::from(s.as_str()))
                    .to_owned(),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    r.extend(xs);
    if let Err(msg) = crate::utils::maybe_warn(t, &r, h) {
        error!("{}", msg);
    };
    Ok(r)
}

pub fn fill_traces(
    v: &Value,
    path: Vec<String>,
    cs: &mut ConstraintSet,
    initiator: &mut Option<&mut String>,
) -> Result<()> {
    match v {
        Value::Object(map) => {
            for (k, v) in map.iter() {
                if k == "Trace" {
                    debug!("Importing {}", path[path.len() - 1]);
                    let mut first_column = String::new();
                    let mut initiator = Some(&mut first_column);
                    fill_traces(v, path.clone(), cs, &mut initiator)?;
                } else {
                    let mut path = path.clone();
                    path.push(k.to_string());
                    fill_traces(v, path, cs, initiator)?;
                }
            }
            Ok(())
        }
        Value::Array(xs) => {
            if path.len() >= 2 {
                let module = path[path.len() - 2].to_string();
                let handle: ColumnRef = Handle::new(&module, &path[path.len() - 1]).into();

                // The min length can be set if the module contains range
                // proofs, that require a minimal length of a certain power of 2
                let module_min_len = cs.columns.min_len.get(&module).cloned().unwrap_or(0);
                let module_spilling = cs.spilling_for_column(&handle);

                if let Result::Ok(Column {
                    t, padding_value, ..
                }) = cs.columns.column(&handle)
                {
                    trace!("inserting {} ({})", handle, xs.len());
                    if let Some(first_column) = initiator.as_mut() {
                        if first_column.is_empty() {
                            first_column.push_str(&handle.pretty());
                        }
                    }

                    let module_spilling = module_spilling
                        .ok_or_else(|| anyhow!("no spilling found for {}", handle.pretty()))?;

                    let mut xs = parse_column(xs, handle.as_handle(), *t)
                        .with_context(|| anyhow!("while importing {}", handle))?;

                    // If the parsed column is not long enought w.r.t. the
                    // minimal module length, prepend it with as many zeroes as
                    // required.
                    // Atomic columns are always padded with zeroes, so there is
                    // no need to trigger a more complex padding system.
                    if xs.len() < module_min_len {
                        xs.reverse();
                        xs.resize_with(module_min_len, || {
                            padding_value.clone().unwrap_or_default()
                        });
                        xs.reverse();
                    }

                    // The first column sets the size of its module
                    let module_raw_size = cs.effective_len_or_set(&module, xs.len() as isize);
                    if xs.len() as isize != module_raw_size {
                        bail!(
                            "{} has an incorrect length: expected {} (from {}), found {}",
                            handle.to_string().blue(),
                            module_raw_size.to_string().red().bold(),
                            initiator.as_ref().unwrap(),
                            xs.len().to_string().yellow().bold(),
                        );
                    }

                    cs.columns.set_column_value(&handle, xs, module_spilling)?
                } else if let Some(Register { magma, .. }) = cs.columns.register(&handle) {
                    let module_spilling = module_spilling
                        .ok_or_else(|| anyhow!("no spilling found for {}", handle.pretty()))?;

                    let mut xs = parse_column(xs, handle.as_handle(), *magma)
                        .with_context(|| anyhow!("while importing {}", handle))?;

                    // If the parsed column is not long enought w.r.t. the
                    // minimal module length, prepend it with as many zeroes as
                    // required.
                    // Atomic columns are always padded with zeroes, so there is
                    // no need to trigger a more complex padding system.
                    if xs.len() < module_min_len {
                        xs.reverse();
                        xs.resize(module_min_len, CValue::zero()); // TODO: register padding values
                        xs.reverse();
                    }

                    let module_raw_size = cs.effective_len_or_set(&module, xs.len() as isize);
                    if xs.len() as isize != module_raw_size {
                        bail!(
                            "{} has an incorrect length: expected {} (from {}), found {}",
                            handle.to_string().blue(),
                            module_raw_size.to_string().red().bold(),
                            initiator.as_ref().unwrap(),
                            xs.len().to_string().yellow().bold(),
                        );
                    }

                    cs.columns
                        .set_register_value(&handle, xs, module_spilling)?
                } else {
                    debug!("ignoring unknown column {}", handle.pretty());
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
