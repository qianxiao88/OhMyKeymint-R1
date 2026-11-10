export type R1Variant = 'full' | 'no-soter' | 'lite'

export function parseVariant(value: string | undefined): R1Variant {
  if (!value) return 'full'
  if (value === 'full' || value === 'no-soter' || value === 'lite') return value
  throw new Error(`Invalid VITE_R1_VARIANT: ${value}`)
}

export const variant = parseVariant(import.meta.env.VITE_R1_VARIANT)
export const IS_LITE = import.meta.env.VITE_R1_VARIANT === 'lite'
export const HAS_SOTER = !import.meta.env.VITE_R1_VARIANT || import.meta.env.VITE_R1_VARIANT === 'full'
export const R1_MODULE_NAME = import.meta.env.VITE_R1_NAME || 'OhMyKeymint-R1-1.3.8-301-full-dev'
export const R1_AUTHOR = '\u6d45\u7b11\u5450'

export function toolAvailable(event: string): boolean {
  if (event === 'openSoterLocal') return HAS_SOTER
  if (!IS_LITE) return true
  return ['cloudUpdate', 'syncSecurityPatch', 'restoreSecurityPatch', 'clearGmsCache',
    'openAppTargets', 'wallpaper', 'feedback'].includes(event)
}
