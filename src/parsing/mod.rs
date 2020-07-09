pub mod parser;

use crate::lexing::lexer;

#[derive(Debug, PartialEq)]
pub enum Statement {
    If {
        condition: Expression,
        if_block: Vec<Statement>,
        else_block: Option<Vec<Statement>>
    },

    FunctionDefinition {
        identifier: String,
        parameters: Vec<(Type, String)>,
        return_type: Option<Type>,
        body: Vec<Statement>
    },

    VariableDeclaration {
        identifier: String,
        var_type: Type,
        value: Option<Expression>
    },

    VariableAssignment {
        identifier: String,
        value: Expression
    }
}

#[derive(Debug, PartialEq)]
pub enum Type {
    Identifier(String),
    Array(Box<Type>)
}

#[derive(Debug, PartialEq)]
pub enum Expression {
    Equal(Box<Expression>, Box<Expression>),
    //NotEqual(Box<Expression>, Box<Expression>),
    GreaterThan(Box<Expression>, Box<Expression>),
    LessThan(Box<Expression>, Box<Expression>),
    Add(Box<Expression>, Box<Expression>),
    Subtract(Box<Expression>, Box<Expression>),
    Multiply(Box<Expression>, Box<Expression>),
    Divide(Box<Expression>, Box<Expression>),

    BooleanNot(Box<Expression>),
    UnaryMinus(Box<Expression>),

    NumberLiteral(lexer::Token),
    StringLiteral(lexer::Token),
    BooleanLiteral(lexer::Token),
    Variable(lexer::Token)
}