use std::collections::HashSet;

use crate::{
    column::{Column, Computation},
    compiler::{ColumnRef, Constraint, ConstraintSet, Expression, Intrinsic, Kind, Node},
    structs::Handle,
};
use anyhow::*;

use super::expression_to_name;

fn invert_expr(e: &Node) -> Node {
    Intrinsic::Inv.call(&[e.to_owned()]).unwrap()
}

/// For all Builtin::Inv encountered, create a new column and the associated constraints
/// pre-computing and proving the inverted column.

impl Node {
    pub(crate) fn do_normalize(
        &mut self,
        get_module: &dyn Fn(&HashSet<ColumnRef>) -> String,
        new_cols: &mut Vec<(Handle, Node)>,
    ) {
        if let Result::Ok(x) = self.pure_eval() {
            *self = Node::from_value(crate::column::Value::try_from(x).unwrap().inverse());
        } else {
            match self.e_mut() {
                Expression::List(es) => {
                    for e in es.iter_mut() {
                        e.do_normalize(get_module, new_cols);
                    }
                }
                Expression::Funcall { func, args, .. } => {
                    for e in args.iter_mut() {
                        e.do_normalize(get_module, new_cols);
                    }
                    if matches!(func, Intrinsic::Normalize) {
                        // Intrinsic::Inv should never have more than one argument
                        assert!(args.len() == 1);
                        let arg = &args[0];
                        if arg.t().is_binary() {
                            // No need for a normalised column if its
                            // already binary.
                            *self = arg.clone();
                        } else if true {
                            let module = get_module(&arg.dependencies());
                            let inverted_handle =
                                Handle::new(module, expression_to_name(arg, "INV"));
                            new_cols.push((inverted_handle.clone(), arg.to_owned()));
                            *self = Intrinsic::Mul
                                .call(&[
                                    arg.to_owned(),
                                    Node::column()
                                        .handle(inverted_handle)
                                        .kind(Kind::Computed)
                                        .t(self.t().m().invert())
                                        .build(),
                                ])
                                .unwrap();
                        } else {
                            todo!("exo-value case");
                            // let module = get_module(&args[0].dependencies());
                            // let normalized_handle =
                            //     Handle::new(module, expression_to_name(&args[0], "NORM"));
                            // new_cols.push((normalized_handle.clone(), args[0].to_owned()));
                            // let old_node = self.clone();
                            // *self = Intrinsic::Mul
                            //     .call(&[
                            //         old_node,
                            //         Node::column()
                            //             .handle(normalized_handle)
                            //             .kind(Kind::Phantom)
                            //             .t(self.t().m().invert())
                            //             .build(),
                            //     ])
                            //     .unwrap()
                            //     .into();
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

impl ConstraintSet {
    pub fn expand_normalizations(&mut self) -> Result<()> {
        let mut new_cols = vec![];

        let get_module = |rs: &HashSet<ColumnRef>| self.columns.module_for(rs.iter()).unwrap();
        for i in 0..self.constraints.len() {
            if let Constraint::Vanishes { expr: e, .. } = self.constraints.get_mut(i).unwrap() {
                e.do_normalize(&get_module, &mut new_cols);
            }
        }

        // TODO: for the exo-normalization case
        // for (normalized_handle, normalized_expr) in new_cols.into_iter() {
        //     if self.columns.by_handle(&normalized_handle).is_err() {
        //         let normalized_id = self.columns.insert_column_and_register(
        //             Column::builder()
        //                 .handle(normalized_handle.clone())
        //                 .kind(Kind::Composite(Box::new(())))
        //                 .build(),
        //         )?;

        //         let inverted_handle = Handle::new(
        //             &normalized_handle.module,
        //             expression_to_name(&normalized_expr, "INV"),
        //         );
        //         let inverted_id = self.columns.insert_column_and_register(
        //             Column::builder()
        //                 .handle(inverted_handle.to_owned())
        //                 .kind(Kind::Composite(Box::new(())))
        //                 .build(),
        //         )?;

        //         self.computations.insert(
        //             &inverted_id,
        //             Computation::Composite {
        //                 target: inverted_id.clone(),
        //                 exp: invert_expr(&normalized_expr),
        //             },
        //         )?;
        //         self.insert_constraint(Constraint::Normalization {
        //             handle: normalized_handle.clone(),
        //             reference: normalized_expr.to_owned(),
        //             inverted: inverted_id,
        //             normalized: normalized_id,
        //         })
        //     }
        // }
        for (inverted_handle, normalized_expr) in new_cols.into_iter() {
            if self.columns.by_handle(&inverted_handle).is_err() {
                let inverted_id = self.columns.insert_column_and_register(
                    Column::builder()
                        .handle(inverted_handle.clone())
                        .kind(Kind::Computed)
                        .build(),
                )?;

                self.computations.insert(
                    &inverted_id,
                    Computation::Composite {
                        target: inverted_id.clone(),
                        exp: invert_expr(&normalized_expr),
                    },
                )?;

                self.constraints.push(Constraint::Normalization {
                    handle: Handle::new(
                        &inverted_handle.module,
                        format!("NORM[{}]", normalized_expr),
                    ),
                    reference: normalized_expr.clone(),
                    inverted: inverted_id,
                })
            }
        }

        Ok(())
    }
}

pub fn expand_invs(cs: &mut ConstraintSet) -> Result<()> {
    if *crate::IS_NATIVE.read().unwrap() {
        cs.expand_normalizations()
    } else {
        Ok(())
    }
}
