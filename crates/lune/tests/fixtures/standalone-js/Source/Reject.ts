export async function RejectExpectedError(): Promise<never> {
    return Promise.reject(new Error("Expected standalone rejection"));
}
