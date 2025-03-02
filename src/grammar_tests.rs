use crate::database::Database;
use crate::database::DbOptions;
use crate::diag::Diagnostic;
use crate::parser::as_str;
use crate::parser::SegmentId;
use crate::parser::StatementAddress;

const GRAMMAR_DB: &[u8] = b"
    $c |- wff class ( ) + = $.
    $( $j syntax 'class'; syntax 'wff'; syntax '|-' as 'wff'; $)
    $v A B $.
    cA $f class A $.
    cB $f class B $.
    weq $a wff A = B $.
    cadd $a class ( A + B ) $.
    ax-com $a |- ( A + B ) = ( B + A ) $.
";

pub(super) fn mkdb(text: &[u8]) -> Database {
    let options = DbOptions {
        incremental: true,
        ..DbOptions::default()
    };
    let mut db = Database::new(options);
    db.parse(
        "test.mm".to_owned(),
        vec![("test.mm".to_owned(), text.to_owned())],
    );
    db
}

#[test]
fn test_lookup() {
    let mut db = mkdb(GRAMMAR_DB);
    let names = db.name_pass();
    assert!(as_str(names.atom_name(names.lookup_symbol(b"A").unwrap().atom)) == "A");
    assert!(as_str(names.atom_name(names.lookup_symbol(b"B").unwrap().atom)) == "B");
    assert!(as_str(names.atom_name(names.lookup_label(b"weq").unwrap().atom)) == "weq");
    assert!(as_str(names.atom_name(names.lookup_label(b"cadd").unwrap().atom)) == "cadd");
}

#[test]
fn test_db_stmt_parse() {
    let mut db = mkdb(GRAMMAR_DB);
    let sset = db.parse_result().clone();
    let grammar = db.grammar_pass().clone();
    let stmt_parse = db.stmt_parse_pass().clone();
    assert!(sset.parse_diagnostics().is_empty());
    assert!(grammar.diagnostics().is_empty());
    assert!(stmt_parse.diagnostics().is_empty());
}

#[test]
fn test_db_formula() {
    let mut db = mkdb(GRAMMAR_DB);
    let stmt_parse = db.stmt_parse_pass().clone();
    let names = db.name_pass().clone();
    {
        let sref = db.statement("ax-com").unwrap();
        let formula = stmt_parse.get_formula(&sref).unwrap();
        assert!(as_str(names.atom_name(formula.get_by_path(&[]).unwrap())) == "weq");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1]).unwrap())) == "cadd");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1, 1]).unwrap())) == "cA");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1, 2]).unwrap())) == "cB");
        assert!(as_str(names.atom_name(formula.get_by_path(&[2]).unwrap())) == "cadd");
        assert!(as_str(names.atom_name(formula.get_by_path(&[2, 1]).unwrap())) == "cB");
        assert!(as_str(names.atom_name(formula.get_by_path(&[2, 2]).unwrap())) == "cA");
    }
}

#[test]
fn test_parse_formula() {
    let mut db = mkdb(GRAMMAR_DB);
    let names = db.name_pass().clone();
    let grammar = db.grammar_pass().clone();
    let wff = names.lookup_symbol(b"wff").unwrap().atom;
    let class = names.lookup_symbol(b"class").unwrap().atom;
    let a = names.lookup_symbol(b"A").unwrap().atom;
    let b = names.lookup_symbol(b"B").unwrap().atom;
    let eq = names.lookup_symbol(b"=").unwrap().atom;
    let plus = names.lookup_symbol(b"+").unwrap().atom;
    let open_parens = names.lookup_symbol(b"(").unwrap().atom;
    let close_parens = names.lookup_symbol(b")").unwrap().atom;
    let fmla_vec = vec![a, eq, open_parens, b, plus, a, close_parens];
    let formula = grammar
        .parse_formula(&mut fmla_vec.clone().into_iter(), &[wff, class], &names)
        .unwrap();
    assert!(as_str(names.atom_name(formula.get_by_path(&[]).unwrap())) == "weq");
    assert!(as_str(names.atom_name(formula.get_by_path(&[1]).unwrap())) == "cA");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2]).unwrap())) == "cadd");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 1]).unwrap())) == "cB");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 2]).unwrap())) == "cA");
    assert!(formula.as_ref(&db).iter().eq(fmla_vec.into_iter()));
}

// This grammar exposes issue #32 in the statement parser
const GRAMMAR_DB_32: &[u8] = b"
    $c |- wff class setvar ( ) = e. |-> $.
    $( $j syntax 'class'; syntax 'setvar'; syntax 'wff'; syntax '|-' as 'wff'; type_conversions; $)
    $v A B C x $.
    cA $f class A $.
    cB $f class B $.
    cC $f class C $.
    vx $f setvar x $.
    cv $a class x $.
    weq $a wff A = B $.
    cov $a class ( A B C ) $.
    cmpt $a class ( x e. A |-> B ) $.
    check $a |- ( x A B ) = C $.
";

#[test]
fn test_db_32_formula() {
    let mut db = mkdb(GRAMMAR_DB_32);
    let stmt_parse = db.stmt_parse_pass().clone();
    let names = db.name_pass().clone();
    {
        let sref = db.statement("check").unwrap();
        let formula = stmt_parse.get_formula(&sref).unwrap();
        assert!(as_str(names.atom_name(formula.get_by_path(&[]).unwrap())) == "weq");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1]).unwrap())) == "cov");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1, 1]).unwrap())) == "cv");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1, 1, 1]).unwrap())) == "vx");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1, 2]).unwrap())) == "cA");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1, 3]).unwrap())) == "cB");
        assert!(as_str(names.atom_name(formula.get_by_path(&[2]).unwrap())) == "cC");
    }
}

// This grammar exposes issue #43 in the statement parser
const GRAMMAR_DB_43: &[u8] = b"
    $c |- wff class setvar ( ) { } = e. | |-> /\\ $.
    $( $j syntax 'class'; syntax 'setvar'; syntax 'wff'; syntax '|-' as 'wff';
          type_conversions; garden_path ( x e. A   =>   ( ph ; $)
    $v ph ps A B x $.
    wph $f wff ph $.
    wps $f wff ps $.
    cA $f class A $.
    cB $f class B $.
    vx $f setvar x $.
    cv $a class x $.
    weq $a wff A = B $.
    wcel $a wff A e. B $.
    wa $a wff ( ph /\\ ps ) $.
    cab $a class { x | ph } $.
    cmpt $a class ( x e. A |-> B ) $.
    check $a |- { x | ( x e. A /\\ ph ) } = B $.
";

#[test]
fn test_db_43_formula() {
    let mut db = mkdb(GRAMMAR_DB_43);
    let stmt_parse = db.stmt_parse_pass().clone();
    let names = db.name_pass().clone();
    {
        let sref = db.statement("check").unwrap();
        let formula = stmt_parse.get_formula(&sref).unwrap();
        assert!(as_str(names.atom_name(formula.get_by_path(&[]).unwrap())) == "weq");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1]).unwrap())) == "cab");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1, 1]).unwrap())) == "vx");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1, 2]).unwrap())) == "wa");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1, 2, 1]).unwrap())) == "wcel");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1, 2, 1, 1]).unwrap())) == "cv");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1, 2, 1, 1, 1]).unwrap())) == "vx");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1, 2, 1, 2]).unwrap())) == "cA");
        assert!(as_str(names.atom_name(formula.get_by_path(&[1, 2, 2]).unwrap())) == "wph");
        assert!(as_str(names.atom_name(formula.get_by_path(&[2]).unwrap())) == "cB");
    }
}

// A minimal set.mm-like database with "Garden Paths"
const GARDEN_PATH_DB: &[u8] = b"
    $c |- wff class setvar { } <. >. , | e. = $.
    $( $j syntax 'class'; syntax 'wff'; syntax '|-' as 'wff';
        type_conversions; garden_path { <.   =>   { A ;
    $)
    $v ph A B C D x y $.
    wph $f wff ph $.
    cA $f class A $.
    cB $f class B $.
    cC $f class C $.
    cD $f class D $.
    vx $f setvar x $.
    vy $f setvar y $.
    cv $a class x $.
    weq $a wff A = B $.
    csn $a class { A } $.
    cop $a class <. A , B >. $.
    copab $a class { <. x , y >. e. A | ph } $.
    formula1 $a |- A = { <. B , C >. } $.
    formula2 $a |- A = { <. x , y >. } $.
    formula3 $a |- A = { <. x , y >. e. B | C = D } $.
";

#[test]
fn test_garden_path_1() {
    let mut db = mkdb(GARDEN_PATH_DB);
    let sset = db.parse_result().clone();
    let stmt_parse = db.stmt_parse_pass().clone();
    let names = db.name_pass().clone();
    assert!(sset.parse_diagnostics().is_empty());
    let sref = db.statement("formula1").unwrap();
    let formula = stmt_parse.get_formula(&sref).unwrap();
    assert!(as_str(names.atom_name(formula.get_by_path(&[]).unwrap())) == "weq");
    assert!(as_str(names.atom_name(formula.get_by_path(&[1]).unwrap())) == "cA");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2]).unwrap())) == "csn");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 1]).unwrap())) == "cop");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 1, 1]).unwrap())) == "cB");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 1, 2]).unwrap())) == "cC");
}

#[test]
fn test_garden_path_2() {
    let mut db = mkdb(GARDEN_PATH_DB);
    let stmt_parse = db.stmt_parse_pass().clone();
    let names = db.name_pass().clone();
    let sref = db.statement("formula2").unwrap();
    let formula = stmt_parse.get_formula(&sref).unwrap();
    assert!(as_str(names.atom_name(formula.get_by_path(&[]).unwrap())) == "weq");
    assert!(as_str(names.atom_name(formula.get_by_path(&[1]).unwrap())) == "cA");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2]).unwrap())) == "csn");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 1]).unwrap())) == "cop");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 1, 1]).unwrap())) == "cv");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 1, 1, 1]).unwrap())) == "vx");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 1, 1]).unwrap())) == "cv");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 1, 2, 1]).unwrap())) == "vy");
}

#[test]
fn test_garden_path_3() {
    let mut db = mkdb(GARDEN_PATH_DB);
    let stmt_parse = db.stmt_parse_pass().clone();
    let names = db.name_pass().clone();
    let sref = db.statement("formula3").unwrap();
    let formula = stmt_parse.get_formula(&sref).unwrap();
    assert!(as_str(names.atom_name(formula.get_by_path(&[]).unwrap())) == "weq");
    assert!(as_str(names.atom_name(formula.get_by_path(&[1]).unwrap())) == "cA");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2]).unwrap())) == "copab");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 1]).unwrap())) == "vx");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 2]).unwrap())) == "vy");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 3]).unwrap())) == "cB");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 4]).unwrap())) == "weq");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 4, 1]).unwrap())) == "cC");
    assert!(as_str(names.atom_name(formula.get_by_path(&[2, 4, 2]).unwrap())) == "cD");
}

macro_rules! sa {
    ($id: expr, $index:expr) => {
        StatementAddress {
            segment_id: SegmentId($id),
            index: $index,
        }
    };
}

macro_rules! grammar_test {
    ($name:ident, $text:expr, $id: expr, $index:expr, $diag:expr) => {
        #[test]
        fn $name() {
            let mut db = mkdb($text);
            let sset = db.parse_result().clone();
            let grammar = db.grammar_pass();
            assert!(sset.parse_diagnostics().is_empty());
            assert_eq!(grammar.diagnostics(), &[(sa!($id, $index), $diag)]);
        }
    };
}

grammar_test!(
    test_missing_float,
    b"$c setvar $. $v x $. vx $a setvar x $.",
    2,
    2,
    Diagnostic::VariableMissingFloat(1)
);
grammar_test!(
    test_ambiguous,
    b"$c A B $. a1 $a A B $. a2 $a A B $.",
    2,
    2,
    Diagnostic::GrammarAmbiguous(sa!(2, 1))
);
grammar_test!(
    test_float_not_var,
    b"$c setvar $. vx $f setvar x $.",
    2,
    1,
    Diagnostic::FloatNotVariable(1)
);
