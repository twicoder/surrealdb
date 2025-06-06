use crate::ctx::{Context, MutableContext};
use crate::dbs::Options;
use crate::doc::CursorDoc;
use crate::err::Error;
use crate::fnc;
use crate::iam::Action;
use crate::sql::fmt::Fmt;
use crate::sql::idiom::Idiom;
use crate::sql::operator::BindingPower;
use crate::sql::script::Script;
use crate::sql::value::Value;
use crate::sql::Permission;
use futures::future::try_join_all;
use reblessive::tree::Stk;
use revision::revisioned;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;

use super::{ControlFlow, FlowResult, FlowResultExt as _, Kind};

pub(crate) const TOKEN: &str = "$surrealdb::private::sql::Function";

#[revisioned(revision = 2)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[serde(rename = "$surrealdb::private::sql::Function")]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[non_exhaustive]
pub enum Function {
	Normal(String, Vec<Value>),
	Custom(String, Vec<Value>),
	Script(Script, Vec<Value>),
	#[revision(
		end = 2,
		convert_fn = "convert_anonymous_arg_computation",
		fields_name = "OldAnonymousFields"
	)]
	Anonymous(Value, Vec<Value>),
	#[revision(start = 2)]
	Anonymous(Value, Vec<Value>, bool),
	// Add new variants here
}

impl Function {
	fn convert_anonymous_arg_computation(
		old: OldAnonymousFields,
		_revision: u16,
	) -> Result<Self, revision::Error> {
		Ok(Function::Anonymous(old.0, old.1, false))
	}
}

pub(crate) enum OptimisedAggregate {
	None,
	Count,
	CountFunction,
	MathMax,
	MathMin,
	MathSum,
	MathMean,
	TimeMax,
	TimeMin,
}

impl PartialOrd for Function {
	#[inline]
	fn partial_cmp(&self, _: &Self) -> Option<Ordering> {
		None
	}
}

impl Function {
	/// Get function name if applicable
	pub fn name(&self) -> Option<&str> {
		match self {
			Self::Normal(n, _) => Some(n.as_str()),
			Self::Custom(n, _) => Some(n.as_str()),
			_ => None,
		}
	}
	/// Get function arguments if applicable
	pub fn args(&self) -> &[Value] {
		match self {
			Self::Normal(_, a) => a,
			Self::Custom(_, a) => a,
			_ => &[],
		}
	}
	/// Convert function call to a field name
	pub fn to_idiom(&self) -> Idiom {
		match self {
			Self::Anonymous(_, _, _) => "function".to_string().into(),
			Self::Script(_, _) => "function".to_string().into(),
			Self::Normal(f, _) => f.to_owned().into(),
			Self::Custom(f, _) => format!("fn::{f}").into(),
		}
	}
	/// Checks if this function invocation is writable
	pub fn writeable(&self) -> bool {
		match self {
			Self::Custom(_, _) => true,
			Self::Script(_, _) => true,
			Self::Normal(f, _) if f == "api::invoke" => true,
			_ => self.args().iter().any(Value::writeable),
		}
	}
	/// Convert this function to an aggregate
	pub fn aggregate(&self, val: Value) -> Result<Self, Error> {
		match self {
			Self::Normal(n, a) => {
				let mut a = a.to_owned();
				match a.len() {
					0 => a.insert(0, val),
					_ => {
						a.remove(0);
						a.insert(0, val);
					}
				}
				Ok(Self::Normal(n.to_owned(), a))
			}
			_ => Err(fail!("Encountered a non-aggregate function: {self:?}")),
		}
	}
	/// Check if this function is a custom function
	pub fn is_custom(&self) -> bool {
		matches!(self, Self::Custom(_, _))
	}

	/// Check if this function is a scripting function
	pub fn is_script(&self) -> bool {
		matches!(self, Self::Script(_, _))
	}

	/// Check if all arguments are static values
	pub fn is_static(&self) -> bool {
		match self {
			Self::Normal(_, a) => a.iter().all(Value::is_static),
			_ => false,
		}
	}

	/// Check if this function is a closure function
	pub fn is_inline(&self) -> bool {
		matches!(self, Self::Anonymous(_, _, _))
	}

	/// Check if this function is a rolling function
	pub fn is_rolling(&self) -> bool {
		match self {
			Self::Normal(f, _) if f == "count" => true,
			Self::Normal(f, _) if f == "math::max" => true,
			Self::Normal(f, _) if f == "math::mean" => true,
			Self::Normal(f, _) if f == "math::min" => true,
			Self::Normal(f, _) if f == "math::sum" => true,
			Self::Normal(f, _) if f == "time::max" => true,
			Self::Normal(f, _) if f == "time::min" => true,
			_ => false,
		}
	}
	/// Check if this function is a grouping function
	pub fn is_aggregate(&self) -> bool {
		match self {
			Self::Normal(f, _) if f == "array::distinct" => true,
			Self::Normal(f, _) if f == "array::first" => true,
			Self::Normal(f, _) if f == "array::flatten" => true,
			Self::Normal(f, _) if f == "array::group" => true,
			Self::Normal(f, _) if f == "array::last" => true,
			Self::Normal(f, _) if f == "count" => true,
			Self::Normal(f, _) if f == "math::bottom" => true,
			Self::Normal(f, _) if f == "math::interquartile" => true,
			Self::Normal(f, _) if f == "math::max" => true,
			Self::Normal(f, _) if f == "math::mean" => true,
			Self::Normal(f, _) if f == "math::median" => true,
			Self::Normal(f, _) if f == "math::midhinge" => true,
			Self::Normal(f, _) if f == "math::min" => true,
			Self::Normal(f, _) if f == "math::mode" => true,
			Self::Normal(f, _) if f == "math::nearestrank" => true,
			Self::Normal(f, _) if f == "math::percentile" => true,
			Self::Normal(f, _) if f == "math::sample" => true,
			Self::Normal(f, _) if f == "math::spread" => true,
			Self::Normal(f, _) if f == "math::stddev" => true,
			Self::Normal(f, _) if f == "math::sum" => true,
			Self::Normal(f, _) if f == "math::top" => true,
			Self::Normal(f, _) if f == "math::trimean" => true,
			Self::Normal(f, _) if f == "math::variance" => true,
			Self::Normal(f, _) if f == "time::max" => true,
			Self::Normal(f, _) if f == "time::min" => true,
			_ => false,
		}
	}
	pub(crate) fn get_optimised_aggregate(&self) -> OptimisedAggregate {
		match self {
			Self::Normal(f, v) if f == "count" => {
				if v.is_empty() {
					OptimisedAggregate::Count
				} else {
					OptimisedAggregate::CountFunction
				}
			}
			Self::Normal(f, _) if f == "math::max" => OptimisedAggregate::MathMax,
			Self::Normal(f, _) if f == "math::mean" => OptimisedAggregate::MathMean,
			Self::Normal(f, _) if f == "math::min" => OptimisedAggregate::MathMin,
			Self::Normal(f, _) if f == "math::sum" => OptimisedAggregate::MathSum,
			Self::Normal(f, _) if f == "time::max" => OptimisedAggregate::TimeMax,
			Self::Normal(f, _) if f == "time::min" => OptimisedAggregate::TimeMin,
			_ => OptimisedAggregate::None,
		}
	}

	pub(crate) fn is_count_all(&self) -> bool {
		matches!(self, Self::Normal(f, p) if f == "count" && p.is_empty() )
	}
}

impl Function {
	/// Process this type returning a computed simple Value
	///
	/// Was marked recursive
	pub(crate) async fn compute(
		&self,
		stk: &mut Stk,
		ctx: &Context,
		opt: &Options,
		doc: Option<&CursorDoc>,
	) -> FlowResult<Value> {
		// Ensure futures are run
		let opt = &opt.new_with_futures(true);
		// Process the function type
		match self {
			Self::Normal(s, x) => {
				// Check this function is allowed
				ctx.check_allowed_function(s)?;
				// Compute the function arguments
				let a = stk
					.scope(|scope| {
						try_join_all(
							x.iter().map(|v| scope.run(|stk| v.compute(stk, ctx, opt, doc))),
						)
					})
					.await?;
				// Run the normal function
				Ok(fnc::run(stk, ctx, opt, doc, s, a).await?)
			}
			Self::Anonymous(v, x, args_computed) => {
				let val = match v {
					c @ Value::Closure(_) => c.clone(),
					Value::Param(p) => ctx.value(p).cloned().unwrap_or(Value::None),
					Value::Block(_) | Value::Subquery(_) | Value::Idiom(_) | Value::Function(_) => {
						stk.run(|stk| v.compute(stk, ctx, opt, doc)).await?
					}
					_ => Value::None,
				};

				match val {
					Value::Closure(closure) => {
						// Compute the function arguments
						let a =
							match args_computed {
								true => x.clone(),
								false => {
									stk.scope(|scope| {
										try_join_all(x.iter().map(|v| {
											scope.run(|stk| v.compute(stk, ctx, opt, doc))
										}))
									})
									.await?
								}
							};

						Ok(stk.run(|stk| closure.compute(stk, ctx, opt, doc, a)).await?)
					}
					v => Err(ControlFlow::from(Error::InvalidFunction {
						name: "ANONYMOUS".to_string(),
						message: format!("'{}' is not a function", v.kindof()),
					})),
				}
			}
			Self::Custom(s, x) => {
				// Get the full name of this function
				let name = format!("fn::{s}");
				// Check this function is allowed
				ctx.check_allowed_function(name.as_str())?;
				// Get the function definition
				let (ns, db) = opt.ns_db()?;
				let val = ctx.tx().get_db_function(ns, db, s).await?;
				// Check permissions
				if opt.check_perms(Action::View)? {
					match &val.permissions {
						Permission::Full => (),
						Permission::None => {
							return Err(ControlFlow::from(Error::FunctionPermissions {
								name: s.to_owned(),
							}))
						}
						Permission::Specific(e) => {
							// Disable permissions
							let opt = &opt.new_with_perms(false);
							// Process the PERMISSION clause
							if !stk.run(|stk| e.compute(stk, ctx, opt, doc)).await?.is_truthy() {
								return Err(ControlFlow::from(Error::FunctionPermissions {
									name: s.to_owned(),
								}));
							}
						}
					}
				}
				// Get the number of function arguments
				let max_args_len = val.args.len();
				// Track the number of required arguments
				let mut min_args_len = 0;
				// Check for any final optional arguments
				val.args.iter().rev().for_each(|(_, kind)| match kind {
					Kind::Option(_) if min_args_len == 0 => {}
					Kind::Any if min_args_len == 0 => {}
					_ => min_args_len += 1,
				});
				// Check the necessary arguments are passed
				if x.len() < min_args_len || max_args_len < x.len() {
					return Err(ControlFlow::from(Error::InvalidArguments {
						name: format!("fn::{}", val.name),
						message: match (min_args_len, max_args_len) {
							(1, 1) => String::from("The function expects 1 argument."),
							(r, t) if r == t => format!("The function expects {r} arguments."),
							(r, t) => format!("The function expects {r} to {t} arguments."),
						},
					}));
				}
				// Compute the function arguments
				let a = stk
					.scope(|scope| {
						try_join_all(
							x.iter().map(|v| scope.run(|stk| v.compute(stk, ctx, opt, doc))),
						)
					})
					.await?;
				// Duplicate context
				let mut ctx = MutableContext::new_isolated(ctx);
				// Process the function arguments
				for (val, (name, kind)) in a.into_iter().zip(&val.args) {
					ctx.add_value(
						name.to_raw(),
						val.coerce_to_kind(kind).map_err(Error::from)?.into(),
					);
				}
				let ctx = ctx.freeze();
				// Run the custom function
				let result =
					stk.run(|stk| val.block.compute(stk, &ctx, opt, doc)).await.catch_return()?;

				if let Some(ref returns) = val.returns {
					result
						.coerce_to_kind(returns)
						.map_err(|e| Error::ReturnCoerce {
							name: val.name.to_string(),
							error: Box::new(e),
						})
						.map_err(ControlFlow::from)
				} else {
					Ok(result)
				}
			}
			#[cfg_attr(not(feature = "scripting"), expect(unused_variables))]
			Self::Script(s, x) => {
				#[cfg(feature = "scripting")]
				{
					// Check if scripting is allowed
					ctx.check_allowed_scripting()?;
					// Compute the function arguments
					let a = stk
						.scope(|scope| {
							try_join_all(
								x.iter().map(|v| scope.run(|stk| v.compute(stk, ctx, opt, doc))),
							)
						})
						.await?;
					// Run the script function
					Ok(fnc::script::run(ctx, opt, doc, s, a).await?)
				}
				#[cfg(not(feature = "scripting"))]
				{
					Err(ControlFlow::Err(Box::new(Error::InvalidScript {
						message: String::from("Embedded functions are not enabled."),
					})))
				}
			}
		}
	}
}

impl fmt::Display for Function {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match self {
			Self::Normal(s, e) => write!(f, "{s}({})", Fmt::comma_separated(e)),
			Self::Custom(s, e) => write!(f, "fn::{s}({})", Fmt::comma_separated(e)),
			Self::Script(s, e) => write!(f, "function({}) {{{s}}}", Fmt::comma_separated(e)),
			Self::Anonymous(p, e, _) => {
				if BindingPower::for_value(p) < BindingPower::Postfix {
					write!(f, "({p})")?;
				} else {
					write!(f, "{p}")?;
				}
				write!(f, "({})", Fmt::comma_separated(e))
			}
		}
	}
}
