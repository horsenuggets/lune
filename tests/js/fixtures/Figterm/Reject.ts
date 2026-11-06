// Import specifiers that cannot be coerced reject the promise instead of throwing.
export function LoadSymbol() {
    return import(Symbol() as unknown as string);
}
