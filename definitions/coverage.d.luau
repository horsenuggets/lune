export type CoverageEntry = {
	["function"]: string,
	lineDefined: number,
	depth: number,
	hits: { number },
}

declare debug: {
	info: (...any) -> ...any,
	traceback: ((string?, number?) -> string) & ((thread, string?, number?) -> string),
	getcoverage: (fn: (...any) -> ...any) -> { CoverageEntry },
}
