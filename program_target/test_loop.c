#include <stdio.h>

int compute(int n) {
    int sum = 0;
    
    for (int i = 0; i < n; i++) {
        sum += i;
        printf("i=%d, sum=%d\n", i, sum);
    }
    
    return sum;
}

int main() {
    printf("Starting loop test\n");
    
    int result = compute(5);
    
    // Conditional
    if (result > 5) {
        printf("Result is large: %d\n", result);
    } else {
        printf("Result is small: %d\n", result);
    }
    
    int count = 3;
    while (count > 0) {
        printf("countdown: %d\n", count);
        count--;
    }
    
    printf("Done! Final result: %d\n", result);
    return 0;
}
