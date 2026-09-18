//! Two structural counts over a span of lines: decision points and nesting depth.
//!
//! Both are read from the syntax tree of the content alone, so a caller can count any
//! version of a file it holds in memory. The spans are the caller's: typically the
//! `start_line..=end_line` of the outline entries it already has.
//!
//! # What is counted
//!
//! A **decision point** is a place where control can go one of two ways. The count follows
//! the usual reading of cyclomatic complexity: every `if` and `else if`, every loop with a
//! condition, every `case` of a switch and every arm of a match, every `catch` or `except`
//! handler, every conditional (ternary) expression, every short-circuit operator (`&&`, `||`,
//! `??`, `and`, `or`), and in Python every `for` and `if` inside a comprehension.
//!
//! It is not a decision point: an `else` branch, a `default` case, a `try` or `finally`
//! block, an unconditional loop (Rust's `loop`), a function, closure or class, an early
//! `return`, `break` or `continue`, optional chaining, or Rust's `?`. Where a grammar gives a
//! wildcard arm no node of its own (`_ =>` in a Rust match, `_ =>` in a C# switch
//! expression) the arm is counted like any other arm.
//!
//! **Nesting depth** is the deepest stack of control structures: `if`, the loops, switch
//! and match, `try`. An `else if` sits at the depth of the `if` it continues, however the
//! grammar nests it. Functions, closures and plain blocks do not deepen it.
//!
//! A node belongs to a span when the line it starts on lies inside the span, so a nested
//! function or closure counts towards the span that holds it.
//!
//! # Languages
//!
//! One table, [`table`], names the node kinds per language. A language with no row has no
//! count: [`span_counts`] returns `None` for it, so a caller reports the count as
//! unavailable and never reads a zero that was not measured.

use crate::lang::outline::outline_language;
use crate::types::Lang;

/// The two counts of one span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpanCounts {
    /// Places where control can go one of two ways, inside the span.
    pub decision_points: u32,
    /// The deepest stack of control structures inside the span; 0 when it has none.
    pub max_nesting: u32,
}

/// The node kinds one language is counted by. Every name is checked against the grammar by
/// a test, so a grammar upgrade that renames a kind fails loudly here.
struct CountTable {
    /// Named node kinds that are one decision point each.
    decisions: &'static [&'static str],
    /// Keyword tokens that are one decision point each, where the grammar gives a case label
    /// no named node of its own.
    decision_tokens: &'static [&'static str],
    /// Named node kinds that hold a binary operator in their `operator` field.
    operator_parents: &'static [&'static str],
    /// Operator tokens that short-circuit, counted only in an `operator` field, so Rust's
    /// `&&x` (a reference to a reference) is never read as one.
    operators: &'static [&'static str],
    /// Named node kinds that deepen nesting.
    nesting: &'static [&'static str],
    /// The `if` kinds, which do not deepen nesting when they continue an `else`.
    branches: &'static [&'static str],
}

/// The languages [`span_counts`] can count.
const COUNTED: &[Lang] = &[
    Lang::Rust,
    Lang::TypeScript,
    Lang::Tsx,
    Lang::JavaScript,
    Lang::Python,
    Lang::Go,
    Lang::Java,
    Lang::CSharp,
    Lang::Php,
];

const ECMASCRIPT: CountTable = CountTable {
    decisions: &[
        "if_statement",
        "for_statement",
        "for_in_statement",
        "while_statement",
        "do_statement",
        "switch_case",
        "catch_clause",
        "ternary_expression",
    ],
    decision_tokens: &[],
    operator_parents: &["binary_expression"],
    operators: &["&&", "||", "??"],
    nesting: &[
        "if_statement",
        "for_statement",
        "for_in_statement",
        "while_statement",
        "do_statement",
        "switch_statement",
        "try_statement",
    ],
    branches: &["if_statement"],
};

/// The one table: which node kinds each language is counted by.
fn table(lang: Lang) -> Option<&'static CountTable> {
    match lang {
        Lang::Rust => Some(&CountTable {
            decisions: &[
                "if_expression",
                "while_expression",
                "for_expression",
                "match_arm",
            ],
            decision_tokens: &[],
            operator_parents: &["binary_expression"],
            operators: &["&&", "||"],
            nesting: &[
                "if_expression",
                "while_expression",
                "for_expression",
                "loop_expression",
                "match_expression",
            ],
            branches: &["if_expression"],
        }),
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => Some(&ECMASCRIPT),
        Lang::Python => Some(&CountTable {
            decisions: &[
                "if_statement",
                "elif_clause",
                "for_statement",
                "while_statement",
                "except_clause",
                "conditional_expression",
                "case_clause",
                "for_in_clause",
                "if_clause",
            ],
            decision_tokens: &[],
            operator_parents: &["boolean_operator"],
            operators: &["and", "or"],
            nesting: &[
                "if_statement",
                "for_statement",
                "while_statement",
                "try_statement",
                "match_statement",
            ],
            branches: &["if_statement"],
        }),
        Lang::Go => Some(&CountTable {
            decisions: &[
                "if_statement",
                "for_statement",
                "expression_case",
                "type_case",
                "communication_case",
            ],
            decision_tokens: &[],
            operator_parents: &["binary_expression"],
            operators: &["&&", "||"],
            nesting: &[
                "if_statement",
                "for_statement",
                "expression_switch_statement",
                "type_switch_statement",
                "select_statement",
            ],
            branches: &["if_statement"],
        }),
        Lang::Java => Some(&CountTable {
            decisions: &[
                "if_statement",
                "for_statement",
                "enhanced_for_statement",
                "while_statement",
                "do_statement",
                "catch_clause",
                "ternary_expression",
            ],
            decision_tokens: &["case"],
            operator_parents: &["binary_expression"],
            operators: &["&&", "||"],
            nesting: &[
                "if_statement",
                "for_statement",
                "enhanced_for_statement",
                "while_statement",
                "do_statement",
                "switch_expression",
                "try_statement",
                "try_with_resources_statement",
            ],
            branches: &["if_statement"],
        }),
        Lang::CSharp => Some(&CountTable {
            decisions: &[
                "if_statement",
                "for_statement",
                "foreach_statement",
                "while_statement",
                "do_statement",
                "catch_clause",
                "conditional_expression",
                "switch_expression_arm",
            ],
            decision_tokens: &["case"],
            operator_parents: &["binary_expression"],
            operators: &["&&", "||", "??"],
            nesting: &[
                "if_statement",
                "for_statement",
                "foreach_statement",
                "while_statement",
                "do_statement",
                "switch_statement",
                "switch_expression",
                "try_statement",
            ],
            branches: &["if_statement"],
        }),
        Lang::Php => Some(&CountTable {
            decisions: &[
                "if_statement",
                "else_if_clause",
                "for_statement",
                "foreach_statement",
                "while_statement",
                "do_statement",
                "case_statement",
                "catch_clause",
                "conditional_expression",
                "match_conditional_expression",
            ],
            decision_tokens: &[],
            operator_parents: &["binary_expression"],
            operators: &["&&", "||", "??", "and", "or", "xor"],
            nesting: &[
                "if_statement",
                "for_statement",
                "foreach_statement",
                "while_statement",
                "do_statement",
                "switch_statement",
                "match_expression",
                "try_statement",
            ],
            branches: &["if_statement"],
        }),
        Lang::Scala
        | Lang::C
        | Lang::Cpp
        | Lang::Ruby
        | Lang::Swift
        | Lang::Kotlin
        | Lang::Elixir
        | Lang::Bash
        | Lang::Dockerfile
        | Lang::Make => None,
    }
}

/// Decision points and nesting depth for each of `spans`, in the same order.
#[must_use]
pub fn span_counts(_content: &str, lang: Lang, _spans: &[(u32, u32)]) -> Option<Vec<SpanCounts>> {
    let _table = table(lang)?;
    let _grammar = outline_language(lang)?;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 1-based inclusive line span of the whole source.
    fn whole(source: &str) -> (u32, u32) {
        (
            1,
            u32::try_from(source.lines().count()).expect("a short fixture"),
        )
    }

    fn counts(source: &str, lang: Lang) -> SpanCounts {
        span_counts(source, lang, &[whole(source)]).expect("the language has a table")[0]
    }

    #[test]
    fn every_kind_the_table_names_exists_in_its_grammar() {
        for &lang in COUNTED {
            let table = table(lang).expect("a counted language has a table");
            let grammar = outline_language(lang).expect("a counted language has a grammar");
            let named = table
                .decisions
                .iter()
                .chain(table.nesting)
                .chain(table.operator_parents)
                .chain(table.branches);
            for kind in named {
                assert_ne!(
                    grammar.id_for_node_kind(kind, true),
                    0,
                    "{lang:?}: the grammar has no named node kind `{kind}`"
                );
            }
            for token in table.operators.iter().chain(table.decision_tokens) {
                assert_ne!(
                    grammar.id_for_node_kind(token, false),
                    0,
                    "{lang:?}: the grammar has no token `{token}`"
                );
            }
        }
    }

    #[test]
    fn a_language_without_a_table_gives_none() {
        assert!(span_counts("x = 1\n", Lang::Ruby, &[(1, 1)]).is_none());
        assert!(span_counts("all:\n\ttrue\n", Lang::Make, &[(1, 2)]).is_none());
    }

    #[test]
    fn straight_line_code_has_no_decision_and_no_nesting() {
        let source = "fn plain(a: u32) -> u32 {\n    let b = a + 1;\n    b * 2\n}\n";
        assert_eq!(
            counts(source, Lang::Rust),
            SpanCounts {
                decision_points: 0,
                max_nesting: 0
            }
        );
    }

    #[test]
    fn rust_counts_branches_loops_arms_and_short_circuits() {
        // if (1), && (1), else-if (1), for (1), the inner if (1), while (1), two match
        // arms (2): 8. Deepest: for > if > while = 3. The else-if sits at its `if`'s depth.
        // `&&x` is a reference, not a short circuit, and `loop` decides nothing.
        let source = concat!(
            "fn busy(a: u32, b: &&str, items: &[u32]) -> u32 {\n",
            "    if a > 1 && a < 9 {\n",
            "        return 1;\n",
            "    } else if a == 0 {\n",
            "        return 2;\n",
            "    }\n",
            "    for item in items {\n",
            "        if *item > a {\n",
            "            while a > 100 {\n",
            "                break;\n",
            "            }\n",
            "        }\n",
            "    }\n",
            "    loop {\n",
            "        break;\n",
            "    }\n",
            "    match b.len() {\n",
            "        0 => 0,\n",
            "        _ => 1,\n",
            "    }\n",
            "}\n",
        );
        assert_eq!(
            counts(source, Lang::Rust),
            SpanCounts {
                decision_points: 8,
                max_nesting: 3
            }
        );
    }

    #[test]
    fn typescript_counts_cases_but_not_default_and_flattens_else_if() {
        // if (1), || (1), else-if (1), ?? twice (2), for-of (1), two cases (2), catch (1),
        // ternary (1): 10. Deepest: try > for > switch = 3.
        let source = concat!(
            "function pick(a: number, b?: string): number {\n",
            "  if (a > 1 || a < -1) {\n",
            "    return 1;\n",
            "  } else if (a === 0) {\n",
            "    return (b ?? '').length;\n",
            "  }\n",
            "  try {\n",
            "    for (const c of b ?? '') {\n",
            "      switch (c) {\n",
            "        case 'x':\n",
            "          return 2;\n",
            "        case 'y':\n",
            "          return 3;\n",
            "        default:\n",
            "          break;\n",
            "      }\n",
            "    }\n",
            "  } catch (e) {\n",
            "    return a > 5 ? 4 : 5;\n",
            "  }\n",
            "  return 0;\n",
            "}\n",
        );
        let expected = SpanCounts {
            decision_points: 10,
            max_nesting: 3,
        };
        assert_eq!(counts(source, Lang::TypeScript), expected);
        assert_eq!(counts(source, Lang::Tsx), expected);
    }

    #[test]
    fn javascript_counts_like_typescript() {
        // if (1), && (1), while (1), do (1), for-in (1): 5. Deepest: while > do = 2.
        let source = concat!(
            "function spin(a, o) {\n",
            "  if (a && o) {\n",
            "    a = 1;\n",
            "  }\n",
            "  while (a < 3) {\n",
            "    do {\n",
            "      a++;\n",
            "    } while (a < 2);\n",
            "  }\n",
            "  for (const k in o) {\n",
            "    a++;\n",
            "  }\n",
            "  return a;\n",
            "}\n",
        );
        assert_eq!(
            counts(source, Lang::JavaScript),
            SpanCounts {
                decision_points: 5,
                max_nesting: 2
            }
        );
    }

    #[test]
    fn python_counts_elif_comprehensions_and_except() {
        // if (1), and (1), elif (1), for (1), comprehension for (1) and if (1),
        // except (1), conditional expression (1), while (1): 9.
        // Deepest: for > try > while = 3. `else` and `finally` decide nothing.
        let source = concat!(
            "def tidy(a, items):\n",
            "    if a > 1 and a < 9:\n",
            "        return 1\n",
            "    elif a == 0:\n",
            "        return 2\n",
            "    else:\n",
            "        pass\n",
            "    for item in items:\n",
            "        try:\n",
            "            while item > a:\n",
            "                item -= 1\n",
            "        except ValueError:\n",
            "            return 3\n",
            "        finally:\n",
            "            pass\n",
            "    evens = [i for i in items if i % 2 == 0]\n",
            "    return 4 if evens else 5\n",
        );
        assert_eq!(
            counts(source, Lang::Python),
            SpanCounts {
                decision_points: 9,
                max_nesting: 3
            }
        );
    }

    #[test]
    fn go_counts_cases_but_not_default_and_flattens_else_if() {
        // if (1), || (1), else-if (1), for (1), two expression cases (2), one select case (1): 7.
        // Deepest: for > switch = 2.
        let source = concat!(
            "package p\n",
            "\n",
            "func pick(a int, c chan int) int {\n",
            "\tif a > 1 || a < -1 {\n",
            "\t\treturn 1\n",
            "\t} else if a == 0 {\n",
            "\t\treturn 2\n",
            "\t}\n",
            "\tfor i := 0; i < a; i++ {\n",
            "\t\tswitch i {\n",
            "\t\tcase 1:\n",
            "\t\t\treturn 3\n",
            "\t\tcase 2:\n",
            "\t\t\treturn 4\n",
            "\t\tdefault:\n",
            "\t\t}\n",
            "\t}\n",
            "\tselect {\n",
            "\tcase v := <-c:\n",
            "\t\treturn v\n",
            "\tdefault:\n",
            "\t}\n",
            "\treturn 0\n",
            "}\n",
        );
        assert_eq!(
            counts(source, Lang::Go),
            SpanCounts {
                decision_points: 7,
                max_nesting: 2
            }
        );
    }

    #[test]
    fn java_counts_case_labels_but_not_default() {
        // if (1), && (1), else-if (1), enhanced for (1), two case labels (2),
        // catch (1), ternary (1), do (1): 9. Deepest: try > for > switch = 3.
        let source = concat!(
            "class P {\n",
            "  int pick(int a, int[] items) {\n",
            "    if (a > 1 && a < 9) {\n",
            "      return 1;\n",
            "    } else if (a == 0) {\n",
            "      return 2;\n",
            "    }\n",
            "    try {\n",
            "      for (int item : items) {\n",
            "        switch (item) {\n",
            "          case 1:\n",
            "            return 3;\n",
            "          case 2:\n",
            "            return 4;\n",
            "          default:\n",
            "            break;\n",
            "        }\n",
            "      }\n",
            "    } catch (RuntimeException e) {\n",
            "      return a > 5 ? 5 : 6;\n",
            "    }\n",
            "    do {\n",
            "      a--;\n",
            "    } while (a > 0);\n",
            "    return 0;\n",
            "  }\n",
            "}\n",
        );
        assert_eq!(
            counts(source, Lang::Java),
            SpanCounts {
                decision_points: 9,
                max_nesting: 3
            }
        );
    }

    #[test]
    fn csharp_counts_case_labels_and_switch_expression_arms() {
        // if (1), || (1), else-if (1), foreach (1), two case labels (2), catch (1),
        // ?? (1), two switch expression arms (2): 10. Deepest: try > foreach > switch = 3.
        let source = concat!(
            "class P {\n",
            "  int Pick(int a, int[] items, string s) {\n",
            "    if (a > 1 || a < -1) {\n",
            "      return 1;\n",
            "    } else if (a == 0) {\n",
            "      return 2;\n",
            "    }\n",
            "    try {\n",
            "      foreach (var item in items) {\n",
            "        switch (item) {\n",
            "          case 1:\n",
            "            return 3;\n",
            "          case 2:\n",
            "            return 4;\n",
            "          default:\n",
            "            break;\n",
            "        }\n",
            "      }\n",
            "    } catch (System.Exception) {\n",
            "      return (s ?? \"\").Length;\n",
            "    }\n",
            "    return a switch { 7 => 5, _ => 6 };\n",
            "  }\n",
            "}\n",
        );
        assert_eq!(
            counts(source, Lang::CSharp),
            SpanCounts {
                decision_points: 10,
                max_nesting: 3
            }
        );
    }

    #[test]
    fn php_counts_elseif_cases_and_match_arms() {
        // if (1), and (1), elseif (1), foreach (1), two cases (2), catch (1),
        // ternary (1), one conditional match arm (1): 9. Deepest: try > foreach > switch = 3.
        let source = concat!(
            "<?php\n",
            "function pick($a, $items) {\n",
            "  if ($a > 1 and $a < 9) {\n",
            "    return 1;\n",
            "  } elseif ($a == 0) {\n",
            "    return 2;\n",
            "  }\n",
            "  try {\n",
            "    foreach ($items as $item) {\n",
            "      switch ($item) {\n",
            "        case 1:\n",
            "          return 3;\n",
            "        case 2:\n",
            "          return 4;\n",
            "        default:\n",
            "          break;\n",
            "      }\n",
            "    }\n",
            "  } catch (Exception $e) {\n",
            "    return $a > 5 ? 5 : 6;\n",
            "  }\n",
            "  return match ($a) { 7 => 8, default => 9 };\n",
            "}\n",
        );
        assert_eq!(
            counts(source, Lang::Php),
            SpanCounts {
                decision_points: 9,
                max_nesting: 3
            }
        );
    }

    #[test]
    fn each_span_is_counted_on_its_own() {
        let source = concat!(
            "fn first(a: u32) -> u32 {\n",
            "    if a > 1 {\n",
            "        return 1;\n",
            "    }\n",
            "    0\n",
            "}\n",
            "\n",
            "fn second(a: u32) -> u32 {\n",
            "    a + 1\n",
            "}\n",
        );
        let got = span_counts(source, Lang::Rust, &[(1, 6), (8, 10), (40, 50)])
            .expect("rust has a table");
        assert_eq!(
            got,
            vec![
                SpanCounts {
                    decision_points: 1,
                    max_nesting: 1
                },
                SpanCounts {
                    decision_points: 0,
                    max_nesting: 0
                },
                SpanCounts {
                    decision_points: 0,
                    max_nesting: 0
                },
            ]
        );
    }

    #[test]
    fn a_long_chain_of_operators_does_not_overflow_the_stack() {
        let mut source = String::from("fn long(a: bool) -> bool {\n    a");
        for _ in 0..20_000 {
            source.push_str(" || a");
        }
        source.push_str("\n}\n");
        assert_eq!(counts(&source, Lang::Rust).decision_points, 20_000);
    }
}
