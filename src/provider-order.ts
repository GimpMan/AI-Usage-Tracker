/**
 * Materialize the order of currently available providers. Persisted positions
 * win, while providers not seen when the order was saved append in their
 * current display order.
 */
export function normalizeProviderOrder(
  savedOrder: readonly string[],
  presentProviders: readonly string[],
): string[] {
  const present = new Set(presentProviders);
  const seen = new Set<string>();
  const normalized: string[] = [];

  for (const provider of savedOrder) {
    if (present.has(provider) && !seen.has(provider)) {
      normalized.push(provider);
      seen.add(provider);
    }
  }

  for (const provider of presentProviders) {
    if (!seen.has(provider)) {
      normalized.push(provider);
      seen.add(provider);
    }
  }

  return normalized;
}

/**
 * Reorder a provider using the full visible order, including providers that
 * have not yet been persisted by a prior drag.
 */
export function reorderProviderOrder(
  savedOrder: readonly string[],
  presentProviders: readonly string[],
  from: string,
  target: string,
  after: boolean,
): string[] {
  const next = normalizeProviderOrder(savedOrder, presentProviders);
  if (from === target) return next;

  const fromIndex = next.indexOf(from);
  const targetIndex = next.indexOf(target);
  if (fromIndex === -1 || targetIndex === -1) return next;

  next.splice(fromIndex, 1);
  const insertionIndex = next.indexOf(target) + (after ? 1 : 0);
  next.splice(insertionIndex, 0, from);
  return next;
}
