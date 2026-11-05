type Unused = string;

export function ThrowExpectedError(): never {
    throw new Error("Expected standalone error");
}
