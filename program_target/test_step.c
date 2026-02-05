#include <stdio.h>

void level_3() {
    int x = 30;
    printf("level_3: x = %d\n", x);
}

void level_2() {
    int x = 20;
    printf("level_2: x = %d\n", x);
    level_3();
    printf("level_2: back from level_3\n");
}

void level_1() {
    int x = 10;          
    printf("level_1: x = %d\n", x);
    level_2();
    printf("level_1: back from level_2\n");
}

int main() {
    printf("main: starting\n");
    level_1();
    printf("main: back from level_1\n");
    printf("main: done\n");
    return 0;
}
