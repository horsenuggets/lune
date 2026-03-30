export type CoverageEntry = {
	Function: string,
	LineDefined: number,
	Depth: number,
	Hits: { number },
}

declare debug: {
	info: (...any) -> ...any,
	traceback: ((string?, number?) -> string) & ((thread, string?, number?) -> string),
	getcoverage: (fn: (...any) -> ...any) -> { CoverageEntry },
	iscoverageenabled: () -> boolean,
}
