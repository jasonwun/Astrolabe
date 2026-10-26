//! A list's columns share the panel width by weight, as Freelens's do (#68).

use astrolabe::projection::{MIN_COLUMN_WIDTH, column_widths, default_column_weights};

#[test]
fn the_name_column_weighs_two_and_every_other_column_one() {
    assert_eq!(default_column_weights(4), vec![2., 1., 1., 1.]);
    assert_eq!(default_column_weights(1), vec![2.]);
    assert!(default_column_weights(0).is_empty());
}

#[test]
fn columns_share_the_width_in_proportion_to_their_weights() {
    assert_eq!(
        column_widths(1000., &[2., 1., 1., 1.]),
        vec![400., 200., 200., 200.]
    );
}

#[test]
fn a_column_below_the_minimum_is_clamped_and_the_rest_share_what_is_left() {
    // HPA at the #67 screenshot's width: Age's 0.5 share (66.875px) is
    // under the minimum, so it takes 80px and the others share 990px.
    assert_eq!(MIN_COLUMN_WIDTH, 80.);
    assert_eq!(
        column_widths(1070., &[1.5, 1.5, 1., 1., 1., 0.5, 1.5]),
        vec![198., 198., 132., 132., 132., 80., 198.]
    );
}

#[test]
fn below_the_sum_of_minimums_every_column_keeps_its_minimum() {
    assert_eq!(column_widths(200., &[2., 1., 1.]), vec![80., 80., 80.]);
}

#[test]
fn whole_pixel_widths_fill_the_width_exactly() {
    let widths = column_widths(1000., &[1., 1., 1.]);
    assert_eq!(widths, vec![333., 333., 334.]);
    assert_eq!(widths.iter().sum::<f32>(), 1000.);
}
