#include <stdio.h>

int main(void) {
    printf("About to segfault...\n");

    int *p = NULL; 
    *p = 42;        

    printf("How am I here?\n");
    return 0;
}
