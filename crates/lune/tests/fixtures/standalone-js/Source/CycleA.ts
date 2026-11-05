import { ReadB } from "./CycleB.js";

export function ReadA(): string {
    return "A";
}

export function ReadCycle(): string {
    return `${ReadA()}${ReadB()}`;
}
