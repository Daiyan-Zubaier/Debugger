/* This will be the target program that this rust debugger will be trying to debug */
#include <stdio.h> 
#include <unistd.h>

int main() { 
  printf("Hello world\n"); 
  
  int num1 = 4; 
  int num2 = 3; 
  int sum = num1 + num2; 
  printf("\nYour sum is: %u\n", sum); 
}