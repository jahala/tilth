export type UserRecord = { id: string; name: string };

export class GenericRepository<T extends { id: string }> {
    constructor(private readonly records: T[]) {}

    findById(id: string): T | undefined {
        return this.records.find((record) => record.id === id);
    }
}

export function loadUser(
    repository: GenericRepository<UserRecord>,
    id: string,
): UserRecord | undefined {
    return repository.findById(id);
}
