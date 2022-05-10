mod parse;
mod variance;

use itertools::Itertools as _;
use smallvec::{smallvec, SmallVec};
use std::borrow::Cow;
use std::cmp;
use std::mem;
use std::ops::{Bound, Deref, RangeBounds};
use std::path::{PathBuf, MAIN_SEPARATOR};

use crate::token::variance::{
    ConjunctiveVariance, Depth, DisjunctiveVariance, IntoInvariantText, Invariance, UnitVariance,
};
use crate::{StrExt as _, PATHS_ARE_CASE_INSENSITIVE};

pub use crate::token::parse::{parse, Annotation, ParseError};
pub use crate::token::variance::{
    invariant_text_prefix, Boundedness, InvariantSize, InvariantText, Variance,
};

pub trait IntoTokens<'t>: Sized {
    type Annotation;

    fn into_tokens(self) -> Vec<Token<'t, Self::Annotation>>;
}

#[derive(Clone, Debug)]
pub struct Tokenized<'t, A = Annotation> {
    expression: Cow<'t, str>,
    tokens: Vec<Token<'t, A>>,
}

impl<'t, A> Tokenized<'t, A> {
    pub fn into_owned(self) -> Tokenized<'static, A> {
        let Tokenized { expression, tokens } = self;
        Tokenized {
            expression: expression.into_owned().into(),
            tokens: tokens.into_iter().map(Token::into_owned).collect(),
        }
    }

    pub fn partition(mut self) -> (PathBuf, Self) {
        // Get the invariant prefix for the token sequence.
        let prefix = variance::invariant_text_prefix(self.tokens.iter()).into();

        // Drain invariant tokens from the beginning of the token sequence and
        // unroot any tokens at the beginning of the sequence (tree wildcards).
        self.tokens
            .drain(0..variance::invariant_text_prefix_upper_bound(&self.tokens));
        self.tokens.first_mut().map(Token::unroot);

        (prefix, self)
    }

    pub fn expression(&self) -> &Cow<'t, str> {
        &self.expression
    }

    pub fn tokens(&self) -> &[Token<'t, A>] {
        &self.tokens
    }

    pub fn variance<T>(&self) -> Variance<T>
    where
        T: Invariance,
        for<'i> &'i Token<'t, A>: UnitVariance<T>,
    {
        self.tokens().iter().conjunctive_variance()
    }
}

impl<'t, A> IntoTokens<'t> for Tokenized<'t, A> {
    type Annotation = A;

    fn into_tokens(self) -> Vec<Token<'t, Self::Annotation>> {
        let Tokenized { tokens, .. } = self;
        tokens
    }
}

#[derive(Clone, Debug)]
pub struct Token<'t, A = Annotation> {
    kind: TokenKind<'t, A>,
    annotation: A,
}

impl<'t, A> Token<'t, A> {
    fn new(kind: TokenKind<'t, A>, annotation: A) -> Self {
        Token { kind, annotation }
    }

    pub fn into_owned(self) -> Token<'static, A> {
        let Token { kind, annotation } = self;
        Token {
            kind: kind.into_owned(),
            annotation,
        }
    }

    pub fn unannotate(self) -> Token<'t, ()> {
        let Token { kind, .. } = self;
        Token {
            kind: kind.unannotate(),
            annotation: (),
        }
    }

    pub fn unroot(&mut self) -> bool {
        self.kind.unroot()
    }

    pub fn kind(&self) -> &TokenKind<'t, A> {
        self.as_ref()
    }

    pub fn annotation(&self) -> &A {
        self.as_ref()
    }

    pub fn has_root(&self) -> bool {
        self.has_preceding_token_with(&mut |token| {
            matches!(
                token.kind(),
                TokenKind::Separator(_) | TokenKind::Wildcard(Wildcard::Tree { has_root: true })
            )
        })
    }

    pub fn has_component_boundary(&self) -> bool {
        self.has_token_with(&mut |token| token.is_component_boundary())
    }

    pub fn has_token_with(&self, f: &mut impl FnMut(&Token<'t, A>) -> bool) -> bool {
        match self.kind() {
            TokenKind::Alternative(ref alternative) => alternative.has_token_with(f),
            TokenKind::Repetition(ref repetition) => repetition.has_token_with(f),
            _ => f(self),
        }
    }

    pub fn has_preceding_token_with(&self, f: &mut impl FnMut(&Token<'t, A>) -> bool) -> bool {
        match self.kind() {
            TokenKind::Alternative(ref alternative) => alternative.has_preceding_token_with(f),
            TokenKind::Repetition(ref repetition) => repetition.has_preceding_token_with(f),
            _ => f(self),
        }
    }

    pub fn has_terminating_token_with(&self, f: &mut impl FnMut(&Token<'t, A>) -> bool) -> bool {
        match self.kind() {
            TokenKind::Alternative(ref alternative) => alternative.has_terminating_token_with(f),
            TokenKind::Repetition(ref repetition) => repetition.has_terminating_token_with(f),
            _ => f(self),
        }
    }
}

impl<'t, A> AsRef<TokenKind<'t, A>> for Token<'t, A> {
    fn as_ref(&self) -> &TokenKind<'t, A> {
        &self.kind
    }
}

impl<'t, A> AsRef<A> for Token<'t, A> {
    fn as_ref(&self) -> &A {
        &self.annotation
    }
}

impl<'t, A> Deref for Token<'t, A> {
    type Target = TokenKind<'t, A>;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<'t> From<TokenKind<'t, ()>> for Token<'t, ()> {
    fn from(kind: TokenKind<'t, ()>) -> Self {
        Token {
            kind,
            annotation: (),
        }
    }
}

impl<'i, 't, A, T> UnitVariance<T> for &'i Token<'t, A>
where
    &'i TokenKind<'t, A>: UnitVariance<T>,
    T: Invariance,
{
    fn unit_variance(self) -> Variance<T> {
        self.kind.unit_variance()
    }
}

#[derive(Clone, Debug)]
pub enum TokenKind<'t, A = ()> {
    Alternative(Alternative<'t, A>),
    Class(Class),
    Literal(Literal<'t>),
    Repetition(Repetition<'t, A>),
    Separator(Separator),
    Wildcard(Wildcard),
}

impl<'t, A> TokenKind<'t, A> {
    pub fn into_owned(self) -> TokenKind<'static, A> {
        match self {
            TokenKind::Alternative(alternative) => alternative.into_owned().into(),
            TokenKind::Class(class) => TokenKind::Class(class),
            TokenKind::Literal(Literal {
                text,
                is_case_insensitive,
            }) => TokenKind::Literal(Literal {
                text: text.into_owned().into(),
                is_case_insensitive,
            }),
            TokenKind::Repetition(repetition) => repetition.into_owned().into(),
            TokenKind::Separator(_) => TokenKind::Separator(Separator),
            TokenKind::Wildcard(wildcard) => TokenKind::Wildcard(wildcard),
        }
    }

    pub fn unannotate(self) -> TokenKind<'t, ()> {
        match self {
            TokenKind::Alternative(alternative) => TokenKind::Alternative(alternative.unannotate()),
            TokenKind::Class(class) => TokenKind::Class(class),
            TokenKind::Literal(literal) => TokenKind::Literal(literal),
            TokenKind::Repetition(repetition) => TokenKind::Repetition(repetition.unannotate()),
            TokenKind::Separator(_) => TokenKind::Separator(Separator),
            TokenKind::Wildcard(wildcard) => TokenKind::Wildcard(wildcard),
        }
    }

    pub fn unroot(&mut self) -> bool {
        match self {
            TokenKind::Wildcard(Wildcard::Tree { ref mut has_root }) => {
                mem::replace(has_root, false)
            },
            _ => false,
        }
    }

    pub fn variance<T>(&self) -> Variance<T>
    where
        T: Invariance,
        for<'i> &'i TokenKind<'t, A>: UnitVariance<T>,
    {
        self.unit_variance()
    }

    pub fn depth(&self) -> Boundedness {
        use crate::token::Wildcard::{One, Tree, ZeroOrMore};
        use TokenKind::{Alternative, Class, Literal, Repetition, Separator, Wildcard};

        match self {
            Class(_) | Literal(_) | Separator(_) | Wildcard(One | ZeroOrMore(_)) => {
                Boundedness::Closed
            },
            Alternative(ref alternative) => {
                if alternative.has_token_with(&mut |token| token.depth().is_open()) {
                    Boundedness::Open
                }
                else {
                    Boundedness::Closed
                }
            },
            Repetition(ref repetition) => {
                let (_, upper) = repetition.bounds();
                if upper.is_none() && repetition.has_component_boundary() {
                    Boundedness::Open
                }
                else {
                    Boundedness::Closed
                }
            },
            Wildcard(Tree { .. }) => Boundedness::Open,
        }
    }

    pub fn breadth(&self) -> Boundedness {
        use crate::token::Wildcard::{One, Tree, ZeroOrMore};
        use TokenKind::{Alternative, Class, Literal, Repetition, Separator, Wildcard};

        match self {
            Class(_) | Literal(_) | Separator(_) | Wildcard(One) => Boundedness::Closed,
            Alternative(ref alternative) => {
                if alternative.has_token_with(&mut |token| token.breadth().is_open()) {
                    Boundedness::Open
                }
                else {
                    Boundedness::Closed
                }
            },
            Repetition(ref repetition) => {
                if repetition.has_token_with(&mut |token| token.breadth().is_open()) {
                    Boundedness::Open
                }
                else {
                    Boundedness::Closed
                }
            },
            Wildcard(Tree { .. } | ZeroOrMore(_)) => Boundedness::Open,
        }
    }

    pub fn has_sub_tokens(&self) -> bool {
        // It is not necessary to detect empty branches or sub-expressions.
        matches!(self, TokenKind::Alternative(_) | TokenKind::Repetition(_))
    }

    pub fn is_component_boundary(&self) -> bool {
        matches!(
            self,
            TokenKind::Separator(_) | TokenKind::Wildcard(Wildcard::Tree { .. })
        )
    }

    #[cfg_attr(not(feature = "diagnostics-inspect"), allow(unused))]
    pub fn is_capturing(&self) -> bool {
        use TokenKind::{Alternative, Class, Repetition, Wildcard};

        matches!(
            self,
            Alternative(_) | Class(_) | Repetition(_) | Wildcard(_),
        )
    }
}

impl<'t, A> From<Alternative<'t, A>> for TokenKind<'t, A> {
    fn from(alternative: Alternative<'t, A>) -> Self {
        TokenKind::Alternative(alternative)
    }
}

impl<A> From<Class> for TokenKind<'_, A> {
    fn from(class: Class) -> Self {
        TokenKind::Class(class)
    }
}

impl<'t, A> From<Repetition<'t, A>> for TokenKind<'t, A> {
    fn from(repetition: Repetition<'t, A>) -> Self {
        TokenKind::Repetition(repetition)
    }
}

impl<A> From<Wildcard> for TokenKind<'static, A> {
    fn from(wildcard: Wildcard) -> Self {
        TokenKind::Wildcard(wildcard)
    }
}

impl<'i, 't, A, T> UnitVariance<T> for &'i TokenKind<'t, A>
where
    &'i Class: UnitVariance<T>,
    &'i Literal<'t>: UnitVariance<T>,
    &'i Separator: UnitVariance<T>,
    T: Invariance,
{
    fn unit_variance(self) -> Variance<T> {
        match self {
            TokenKind::Alternative(ref alternative) => alternative.unit_variance(),
            TokenKind::Class(ref class) => class.unit_variance(),
            TokenKind::Literal(ref literal) => literal.unit_variance(),
            TokenKind::Repetition(ref repetition) => repetition.unit_variance(),
            TokenKind::Separator(ref separator) => separator.unit_variance(),
            TokenKind::Wildcard(_) => Variance::Variant(Boundedness::Open),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Alternative<'t, A = ()>(Vec<Vec<Token<'t, A>>>);

impl<'t, A> Alternative<'t, A> {
    pub fn into_owned(self) -> Alternative<'static, A> {
        Alternative(
            self.0
                .into_iter()
                .map(|tokens| tokens.into_iter().map(Token::into_owned).collect())
                .collect(),
        )
    }

    pub fn unannotate(self) -> Alternative<'t, ()> {
        let Alternative(branches) = self;
        Alternative(
            branches
                .into_iter()
                .map(|branch| branch.into_iter().map(|token| token.unannotate()).collect())
                .collect(),
        )
    }

    pub fn branches(&self) -> &Vec<Vec<Token<'t, A>>> {
        &self.0
    }

    pub fn has_token_with(&self, f: &mut impl FnMut(&Token<'t, A>) -> bool) -> bool {
        self.0
            .iter()
            .any(|tokens| tokens.iter().any(|token| token.has_token_with(f)))
    }

    pub fn has_preceding_token_with(&self, f: &mut impl FnMut(&Token<'t, A>) -> bool) -> bool {
        self.0.iter().any(|tokens| {
            tokens
                .first()
                .map(|token| token.has_preceding_token_with(f))
                .unwrap_or(false)
        })
    }

    pub fn has_terminating_token_with(&self, f: &mut impl FnMut(&Token<'t, A>) -> bool) -> bool {
        self.0.iter().any(|tokens| {
            tokens
                .last()
                .map(|token| token.has_terminating_token_with(f))
                .unwrap_or(false)
        })
    }
}

impl<'t, A> From<Vec<Vec<Token<'t, A>>>> for Alternative<'t, A> {
    fn from(alternatives: Vec<Vec<Token<'t, A>>>) -> Self {
        Alternative(alternatives)
    }
}

impl<'i, 't, A, T> UnitVariance<T> for &'i Alternative<'t, A>
where
    T: Invariance,
    &'i Token<'t, A>: UnitVariance<T>,
{
    fn unit_variance(self) -> Variance<T> {
        self.branches()
            .iter()
            .map(|tokens| tokens.iter().conjunctive_variance())
            .disjunctive_variance()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Archetype {
    Character(char),
    Range(char, char),
}

impl Archetype {
    fn domain_variance(&self) -> Variance<char> {
        match self {
            Archetype::Character(x) => {
                if PATHS_ARE_CASE_INSENSITIVE {
                    Variance::Variant(Boundedness::Closed)
                }
                else {
                    Variance::Invariant(*x)
                }
            },
            Archetype::Range(a, b) => {
                if (a != b) || PATHS_ARE_CASE_INSENSITIVE {
                    Variance::Variant(Boundedness::Closed)
                }
                else {
                    Variance::Invariant(*a)
                }
            },
        }
    }
}

impl From<char> for Archetype {
    fn from(literal: char) -> Self {
        Archetype::Character(literal)
    }
}

impl From<(char, char)> for Archetype {
    fn from(range: (char, char)) -> Self {
        Archetype::Range(range.0, range.1)
    }
}

impl<'i, 't> UnitVariance<InvariantText<'t>> for &'i Archetype {
    fn unit_variance(self) -> Variance<InvariantText<'t>> {
        self.domain_variance()
            .map_invariance(|invariance| invariance.to_string().into_nominal_text())
    }
}

impl<'i> UnitVariance<InvariantSize> for &'i Archetype {
    fn unit_variance(self) -> Variance<InvariantSize> {
        // This is pessimistic and assumes that the code point will require four
        // bytes when encoded as UTF-8. This is technically possible, but most
        // commonly only one or two bytes will be required.
        self.domain_variance().map_invariance(|_| 4.into())
    }
}

#[derive(Clone, Debug)]
pub struct Class {
    is_negated: bool,
    archetypes: Vec<Archetype>,
}

impl Class {
    pub fn archetypes(&self) -> &[Archetype] {
        &self.archetypes
    }

    pub fn is_negated(&self) -> bool {
        self.is_negated
    }
}

impl<'i, T> UnitVariance<T> for &'i Class
where
    &'i Archetype: UnitVariance<T>,
    T: Invariance,
{
    fn unit_variance(self) -> Variance<T> {
        if self.is_negated {
            // It is not feasible to encode a character class that matches all
            // UTF-8 text and therefore nothing when negated, and so a character
            // class must be variant if it is negated.
            Variance::Variant(Boundedness::Closed)
        }
        else {
            // TODO: This ignores casing groups, such as in the pattern `[aA]`.
            self.archetypes().iter().disjunctive_variance()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Evaluation {
    Eager,
    Lazy,
}

#[derive(Clone, Debug)]
pub struct Literal<'t> {
    text: Cow<'t, str>,
    is_case_insensitive: bool,
}

impl<'t> Literal<'t> {
    pub fn text(&self) -> &str {
        self.text.as_ref()
    }

    fn domain_variance(&self) -> Variance<&Cow<'t, str>> {
        if self.has_variant_casing() {
            Variance::Variant(Boundedness::Closed)
        }
        else {
            Variance::Invariant(&self.text)
        }
    }

    pub fn is_case_insensitive(&self) -> bool {
        self.is_case_insensitive
    }

    pub fn has_variant_casing(&self) -> bool {
        // If path case sensitivity agrees with the literal case sensitivity,
        // then the literal is not variant. Otherwise, the literal is variant if
        // it contains characters with casing.
        (PATHS_ARE_CASE_INSENSITIVE != self.is_case_insensitive) && self.text.has_casing()
    }
}

impl<'i, 't> UnitVariance<InvariantText<'t>> for &'i Literal<'t> {
    fn unit_variance(self) -> Variance<InvariantText<'t>> {
        self.domain_variance()
            .map_invariance(|invariance| invariance.clone().into_nominal_text())
    }
}

impl<'i, 't> UnitVariance<InvariantSize> for &'i Literal<'t> {
    fn unit_variance(self) -> Variance<InvariantSize> {
        self.domain_variance()
            .map_invariance(|invariance| invariance.as_bytes().len().into())
    }
}

#[derive(Clone, Debug)]
pub struct Repetition<'t, A = ()> {
    tokens: Vec<Token<'t, A>>,
    lower: usize,
    step: Option<usize>,
}

impl<'t, A> Repetition<'t, A> {
    fn new(tokens: Vec<Token<'t, A>>, bounds: impl RangeBounds<usize>) -> Option<Self> {
        let lower = match bounds.start_bound() {
            Bound::Included(lower) => *lower,
            Bound::Excluded(lower) => *lower + 1,
            Bound::Unbounded => 0,
        };
        let upper = match bounds.end_bound() {
            Bound::Included(upper) => Some(*upper),
            Bound::Excluded(upper) => Some(cmp::max(*upper, 1) - 1),
            Bound::Unbounded => None,
        };
        match upper {
            Some(upper) => (upper != 0 && upper >= lower).then(|| Repetition {
                tokens,
                lower,
                step: Some(upper - lower),
            }),
            None => Some(Repetition {
                tokens,
                lower,
                step: None,
            }),
        }
    }

    pub fn into_owned(self) -> Repetition<'static, A> {
        let Repetition {
            tokens,
            lower,
            step,
        } = self;
        Repetition {
            tokens: tokens.into_iter().map(Token::into_owned).collect(),
            lower,
            step,
        }
    }

    pub fn unannotate(self) -> Repetition<'t, ()> {
        let Repetition {
            tokens,
            lower,
            step,
        } = self;
        Repetition {
            tokens: tokens.into_iter().map(|token| token.unannotate()).collect(),
            lower,
            step,
        }
    }

    pub fn tokens(&self) -> &Vec<Token<'t, A>> {
        &self.tokens
    }

    pub fn bounds(&self) -> (usize, Option<usize>) {
        (self.lower, self.step.map(|step| self.lower + step))
    }

    pub fn has_component_boundary(&self) -> bool {
        self.has_token_with(&mut |token| token.is_component_boundary())
    }

    pub fn has_token_with(&self, f: &mut impl FnMut(&Token<'t, A>) -> bool) -> bool {
        self.tokens.iter().any(|token| token.has_token_with(f))
    }

    pub fn has_preceding_token_with(&self, f: &mut impl FnMut(&Token<'t, A>) -> bool) -> bool {
        self.tokens
            .first()
            .map(|token| token.has_preceding_token_with(f))
            .unwrap_or(false)
    }

    pub fn has_terminating_token_with(&self, f: &mut impl FnMut(&Token<'t, A>) -> bool) -> bool {
        self.tokens
            .last()
            .map(|token| token.has_terminating_token_with(f))
            .unwrap_or(false)
    }
}

impl<'i, 't, A, T> UnitVariance<T> for &'i Repetition<'t, A>
where
    T: Invariance,
    &'i Token<'t, A>: UnitVariance<T>,
{
    fn unit_variance(self) -> Variance<T> {
        use Boundedness::Open;
        use TokenKind::Separator;
        use Variance::Variant;

        let variance = self
            .tokens()
            .iter()
            // Coalesce tokens with open variance with separators. This isn't
            // destructive and doesn't affect invariant strings, because this
            // only happens in the presence of open variance, which means that
            // the repetition has no invariant string representation.
            .coalesce(|left, right| {
                match (
                    (left.kind(), left.unit_variance()),
                    (right.kind(), right.unit_variance()),
                ) {
                    ((Separator(_), _), (_, Variant(Open))) => Ok(right),
                    ((_, Variant(Open)), (Separator(_), _)) => Ok(left),
                    _ => Err((left, right)),
                }
            })
            .conjunctive_variance();
        match self.step {
            // TODO: Repeating the invariant text could be unknowingly expensive
            //       and could pose a potential attack vector. Is there an
            //       alternative representation that avoids epic allocations?
            Some(0) => variance.map_invariance(|invariance| invariance * self.lower),
            Some(_) | None => variance + Variant(Open),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Separator;

impl Separator {
    pub fn invariant_text() -> String {
        MAIN_SEPARATOR.to_string()
    }
}

impl<'i, 't> UnitVariance<InvariantText<'t>> for &'i Separator {
    fn unit_variance(self) -> Variance<InvariantText<'t>> {
        Variance::Invariant(Separator::invariant_text().into_structural_text())
    }
}

impl<'i> UnitVariance<InvariantSize> for &'i Separator {
    fn unit_variance(self) -> Variance<InvariantSize> {
        Variance::Invariant(Separator::invariant_text().as_bytes().len().into())
    }
}

#[derive(Clone, Debug)]
pub enum Wildcard {
    One,
    ZeroOrMore(Evaluation),
    Tree { has_root: bool },
}

#[derive(Clone, Debug)]
pub struct LiteralSequence<'i, 't>(SmallVec<[&'i Literal<'t>; 4]>);

impl<'i, 't> LiteralSequence<'i, 't> {
    pub fn literals(&self) -> &[&'i Literal<'t>] {
        self.0.as_ref()
    }

    pub fn text(&self) -> Cow<'t, str> {
        if self.literals().len() == 1 {
            self.literals().first().unwrap().text.clone()
        }
        else {
            self.literals()
                .iter()
                .map(|literal| &literal.text)
                .join("")
                .into()
        }
    }

    #[cfg(any(unix, windows))]
    pub fn is_semantic_literal(&self) -> bool {
        matches!(self.text().as_ref(), "." | "..")
    }

    #[cfg(not(any(unix, windows)))]
    pub fn is_semantic_literal(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct Component<'i, 't, A = ()>(SmallVec<[&'i Token<'t, A>; 4]>);

impl<'i, 't, A> Component<'i, 't, A> {
    pub fn tokens(&self) -> &[&'i Token<'t, A>] {
        self.0.as_ref()
    }

    pub fn literal(&self) -> Option<LiteralSequence<'i, 't>> {
        // This predicate is more easily expressed with `all`, but `any` is used
        // here, because it returns `false` for empty iterators and in that case
        // this function should return `None`.
        (!self
            .tokens()
            .iter()
            .any(|token| !matches!(token.kind(), TokenKind::Literal(_))))
        .then(|| {
            LiteralSequence(
                self.tokens()
                    .iter()
                    .map(|token| match token.kind() {
                        TokenKind::Literal(ref literal) => literal,
                        _ => unreachable!(), // See predicate above.
                    })
                    .collect(),
            )
        })
    }

    pub fn variance<T>(&self) -> Variance<T>
    where
        T: Invariance,
        &'i Token<'t, A>: UnitVariance<T>,
    {
        self.tokens().iter().copied().conjunctive_variance()
    }

    pub fn depth(&self) -> Boundedness {
        self.tokens().iter().copied().depth()
    }
}

impl<'i, 't, A> Clone for Component<'i, 't, A> {
    fn clone(&self) -> Self {
        Component(self.0.clone())
    }
}

pub fn any<'t, A, I>(tokens: I) -> Token<'t, ()>
where
    I: IntoIterator,
    I::Item: IntoIterator<Item = Token<'t, A>>,
{
    Token {
        kind: Alternative(
            tokens
                .into_iter()
                .map(|tokens| tokens.into_iter().map(|token| token.unannotate()).collect())
                .collect(),
        )
        .into(),
        annotation: (),
    }
}

pub fn components<'i, 't, A, I>(tokens: I) -> impl Iterator<Item = Component<'i, 't, A>>
where
    't: 'i,
    A: 't,
    I: IntoIterator<Item = &'i Token<'t, A>>,
{
    tokens.into_iter().peekable().batching(|tokens| {
        let mut first = tokens.next();
        while matches!(first.map(Token::kind), Some(TokenKind::Separator(_))) {
            first = tokens.next();
        }
        first.map(|first| match first.kind() {
            TokenKind::Wildcard(Wildcard::Tree { .. }) => Component(smallvec![first]),
            _ => Component(
                Some(first)
                    .into_iter()
                    .chain(tokens.peeking_take_while(|token| !token.is_component_boundary()))
                    .collect(),
            ),
        })
    })
}

// TODO: This implementation allocates many `Vec`s.
pub fn literals<'i, 't, A, I>(
    tokens: I,
) -> impl Iterator<Item = (Component<'i, 't, A>, LiteralSequence<'i, 't>)>
where
    't: 'i,
    A: 't,
    I: IntoIterator<Item = &'i Token<'t, A>>,
{
    components(tokens).flat_map(|component| {
        if let Some(literal) = component.literal() {
            vec![(component, literal)]
        }
        else {
            component
                .tokens()
                .iter()
                .filter_map(|token| match token.kind() {
                    TokenKind::Alternative(ref alternative) => Some(
                        alternative
                            .branches()
                            .iter()
                            .flat_map(literals)
                            .collect::<Vec<_>>(),
                    ),
                    TokenKind::Repetition(ref repetition) => {
                        Some(literals(repetition.tokens()).collect::<Vec<_>>())
                    },
                    _ => None,
                })
                .flatten()
                .collect::<Vec<_>>()
        }
    })
}

#[cfg(test)]
mod tests {
    use crate::token::{self, TokenKind};

    #[test]
    fn literal_case_insensitivity() {
        let tokenized = token::parse("(?-i)../foo/(?i)**/bar/**(?-i)/baz/*(?i)qux").unwrap();
        let literals: Vec<_> = tokenized
            .tokens()
            .iter()
            .flat_map(|token| match token.kind {
                TokenKind::Literal(ref literal) => Some(literal),
                _ => None,
            })
            .collect();

        assert!(!literals[0].is_case_insensitive); // `..`
        assert!(!literals[1].is_case_insensitive); // `foo`
        assert!(literals[2].is_case_insensitive); // `bar`
        assert!(!literals[3].is_case_insensitive); // `baz`
        assert!(literals[4].is_case_insensitive); // `qux`
    }
}
