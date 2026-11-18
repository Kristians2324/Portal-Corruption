fn tick_pre(game: &mut Game<'_>, dimension: &str) {
    rift_runtime::apply_pending(game, dimension);
}
