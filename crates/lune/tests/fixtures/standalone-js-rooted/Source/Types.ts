export type TerminalSize = {
    Columns: number;
    Rows: number;
};

export type Story = {
    Name: string;
    Projects: string[];
    Terminal: TerminalSize;
};
