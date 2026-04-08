### The prompt to get 4 units to test their output against the real answer
create exactly these units:

```
// command unit one
export type UserType = "admin" | "user";

export type UT = 'Commander' | 'Soldier';
```

```
// query unit one
import type { UserType as UT } from '@units/command one unit path';
export type UserData = {
    type: UT,
    name: string,
}

export type UserType = UT | 'Staff' | 'User';
```

```
// gateway unit one
import type { UserType } from '@units/command one unit path';
import type { UserData } from '@units/query one unit path';

type UserDataWrapped = {
    wrappedType: UserType,
    timestamp: number,
    usersData: UserData[],
}

export { UserDataWrapped };
export type UT = 'Guard' | 'Security';
```

```
// api unit one (POST)
import { UserDataWrapped as UDW } from '@units/gateway one unit path';
import type { UserType } from '@units/command one unit path';
import { UserType as UXType } from '@units/query one unit path';

type UT = 'Post' | 'Get';

export type RequestBody = {
    targetUserType: UserType,
    targetUXType: UXType,
    name?: string,
    innerUT: UT,
}

export type ResponseBody = {
    data: UDW,
    total: number,
    targetUserType: UserType,
    targetUXType: UXType,
    name?: string,
    innerUT2: UT,
}

export type QueryParams = never;
```


IMPORTANT: JUST WRITE THE EXACT CODE I WROTE, the unit path are samples, use correct unit paths (like {context}.{module}.{unit_type}.{unit_slug}

Note: This is just a test to check the infrastructure, I know some codes might fail, just write the exact code I wrote. use context "XXXXXXXXX", module "infra-test" and slugs as you NamedNodeMap.


### The expected output for the API unit's `RequestBody` and `ResponseBody`

```ts
// API unit table row > RequestBody must match Root___RequestBody
type Root___UT = 'Post' | 'Get';
type Pimo_Infra_Test_Command_Unit_One___UserType = "admin" | "user";
type Pimo_Infra_Test_Query_Unit_One___UserType = Pimo_Infra_Test_Command_Unit_One___UserType | 'Staff' | 'User';
type Root___RequestBody = {
    targetUserType: Pimo_Infra_Test_Command_Unit_One___UserType,
    targetUXType: Pimo_Infra_Test_Query_Unit_One___UserType,
    name?: string,
    innerUT: Root___UT,
};
```

```ts
// API unit table row > ResponseBody must match Root___ResponseBody
type Pimo_Infra_Test_Command_Unit_One___UserType = "admin" | "user";
type Pimo_Infra_Test_Query_Unit_One___UserData = {
    type: Pimo_Infra_Test_Command_Unit_One___UserType,
    name: string,
};
type Pimo_Infra_Test_Gateway_Unit_One___UserDataWrapped = {
    wrappedType: Pimo_Infra_Test_Command_Unit_One___UserType,
    timestamp: number,
    usersData: Pimo_Infra_Test_Query_Unit_One___UserData[],
};
type Pimo_Infra_Test_Query_Unit_One___UserType = Pimo_Infra_Test_Command_Unit_One___UserType | 'Staff' | 'User';
type Root___UT = 'Post' | 'Get';
type Root___ResponseBody = {
    data: Pimo_Infra_Test_Gateway_Unit_One___UserDataWrapped,
    total: number,
    targetUserType: Pimo_Infra_Test_Command_Unit_One___UserType,
    targetUXType: Pimo_Infra_Test_Query_Unit_One___UserType,
    name?: string,
    innerUT2: Root___UT,
};
```
