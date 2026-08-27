public class MyPositionData
{
	public int Altitude
	{
		get { return e; }
	}

	public byte MyPositionChannel
	{
		get { return f; }
	}

	public override void ax(m6 A_0, int A_1)
	{
		int num = 4384 + 32 * A_1;
		A_0.a(base.c, num + 12);
		A_0.b(e, 4, num);
		A_0.a(base.g, 2, num + 12);
		A_0.a(j, num + 4);
		A_0.a(m, num + 5);
		A_0.b(p, 2, num + 6);
		A_0.a(s, 3, num + 12);
		A_0.a(v, num + 8);
		A_0.a(y, num + 9);
		A_0.b(ab, 2, num + 10);
		A_0.a(f, num + 13);
		A_0.c(base.e, num + 14, nb.aa);
	}

	public override void ay(m6 A_0, int A_1)
	{
		int num = 4384 + 32 * A_1;
		e = A_0.h(num, 4);
	}
}
