// Copyright 2015-2022 Swim Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::comparator::{compare_values, incremental_compare};
use crate::parser::{ParseError, ParseIterator, Span};
use std::borrow::Cow;
use swim_form::structural::read::event::ReadEvent;
use swim_model::Value;

fn value_from_string(rep: &str) -> Result<Value, ParseError> {
    let span = Span::new(rep);
    crate::parser::parse_recognize(span, false)
}

#[test]
fn cmp_simple() {
    let first = "@name(a: 1, b: 2)";
    let second = "@name(a: 1, b: 2)";

    assert!(compare_values(first, second));

    let result_1 = value_from_string(first).unwrap();
    let result_2 = value_from_string(second).unwrap();
    assert_eq!(result_1, result_2);
}

#[test]
fn cmp_complex() {
    let first = "{a:2}";
    let second = "{ a: 2 }";

    assert!(compare_values(first, second));

    let result_1 = value_from_string(first).unwrap();
    let result_2 = value_from_string(second).unwrap();
    assert_eq!(result_1, result_2);

    let first = "@tag(){}:1";
    let second = "@tag{}:1";

    assert!(compare_values(first, second));

    let result_1 = value_from_string(first).unwrap();
    let result_2 = value_from_string(second).unwrap();
    assert_eq!(result_1, result_2);

    let first = "@name(a: 1, b: 2)";
    let second = "@name({a: 1, b: 2})";

    assert!(compare_values(first, second));

    let result_1 = value_from_string(first).unwrap();
    let result_2 = value_from_string(second).unwrap();
    assert_eq!(result_1, result_2);
}

#[test]
fn cmp_early_termination_simple() {
    let first = "@name(a: 1, b: 2, c: 3)";
    let second = "@name(a:1, b: 4, c: 3)";

    let first_iter = &mut ParseIterator::new(Span::new(first), false);
    let second_iter = &mut ParseIterator::new(Span::new(second), false);

    assert!(!incremental_compare(first_iter, second_iter));
    assert_eq!(
        first_iter.next().unwrap().unwrap(),
        ReadEvent::TextValue(Cow::from("c"))
    );
    assert_eq!(
        second_iter.next().unwrap().unwrap(),
        ReadEvent::TextValue(Cow::from("c"))
    );

    let result_1 = value_from_string(first).unwrap();
    let result_2 = value_from_string(second).unwrap();
    assert_ne!(result_1, result_2);
}

#[test]
fn cmp_early_termination_complex() {
    let first = "@name(a: 1, b: 2)";
    let second = "@name(   {a: 3, b: 2}    )";

    let first_iter = &mut ParseIterator::new(Span::new(first), false);
    let second_iter = &mut ParseIterator::new(Span::new(second), false);

    assert!(!incremental_compare(first_iter, second_iter));
    assert_eq!(
        first_iter.next().unwrap().unwrap(),
        ReadEvent::TextValue(Cow::from("b"))
    );
    assert_eq!(
        second_iter.next().unwrap().unwrap(),
        ReadEvent::TextValue(Cow::from("b"))
    );

    let result_1 = value_from_string(first).unwrap();
    let result_2 = value_from_string(second).unwrap();
    assert_ne!(result_1, result_2);
}

#[test]
fn cmp_early_termination_invalid() {
    let first = vec![Ok(ReadEvent::Slot)];
    let second = vec![Ok(ReadEvent::Slot)];

    let mut first_iter = first.into_iter();
    let mut second_iter = second.into_iter();

    assert!(!incremental_compare(&mut first_iter, &mut second_iter));
    // assert_eq!(
    //     first_iter.next().unwrap().unwrap(),
    //     ReadEvent::TextValue(Cow::from("c"))
    // );
    // assert_eq!(
    //     second_iter.next().unwrap().unwrap(),
    //     ReadEvent::TextValue(Cow::from("c"))
    // );
}
