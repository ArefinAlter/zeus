// A small seeded generator, so the sample data is the same on every start and in every screenshot.

export function rng(seed: number): () => number {
  let s = seed >>> 0
  return () => {
    s = (s + 0x6d2b79f5) >>> 0
    let t = s
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

export function pick<T>(r: () => number, list: readonly T[]): T {
  return list[Math.floor(r() * list.length)] as T
}

const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'

/** A prefixed ULID-shaped id (`job_01J...`), from a timestamp and the generator. */
export function ulid(prefix: string, t: number, r: () => number = Math.random): string {
  let time = ''
  let n = t
  for (let i = 0; i < 10; i++) {
    time = CROCKFORD[n % 32] + time
    n = Math.floor(n / 32)
  }
  let rand = ''
  for (let i = 0; i < 16; i++) rand += CROCKFORD[Math.floor(r() * 32)]
  return `${prefix}_${time}${rand}`
}
