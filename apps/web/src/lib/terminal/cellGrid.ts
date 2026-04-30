/**
 * Wire-side cell-grid bounds — same as `relayterm_protocol::ResizeMsg`.
 *
 * Lives outside both `lib/dev/` and `lib/api/` so neither layer depends
 * on the other. The backend already rejects out-of-range values with
 * `invalid_input`; the front-end uses these constants to refuse before
 * sending a wire frame so a typo in the launcher UI can't generate a
 * round-trip just to learn `81 != "eighty-one"`.
 */
export const CELL_GRID_MIN = 1;
export const CELL_GRID_MAX = 4096;
