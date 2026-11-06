import { ReadA } from "./CycleA.js";

export function ReadB(): string {
    return "B";
}

export function ReadReverseCycle(): string {
    return `${ReadA()}${ReadB()}`;
}
