// Build-variant flags. `build.py` passes VITE_R1_VARIANT to the Vite build, so
// these are constant-folded per package and the branches they guard are dropped.
const VARIANT = import.meta.env.VITE_R1_VARIANT

export const IS_LITE = VARIANT === 'lite'
export const HAS_SOTER = VARIANT === undefined || VARIANT === 'full'
